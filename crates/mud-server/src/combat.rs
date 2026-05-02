use bevy_ecs::prelude::*;
use mud_world::{
    AppliedTo, CombatStats, Corpse, CorpseDecay, Description, EffectInstance, Exits, Fighting,
    Ghost, Guarding, Health, Item, Keywords, KnownAbilities, Located, Mob, MobPrototypes, Named,
    Player, PlayerFlags, Posture, PostureKind, Slot, Stunned, Wealth, WearableIn, WorldKey,
    WorldKeyIndex,
};
use tracing::info;

use crate::TickCount;
use crate::commands::{
    apply_damage, broadcast_room_except_players_rendered, broadcast_room_except_rendered,
    cmd_flee, color_mode_for, direction_name, disengage_attackers_of, drain_stamina, name_of,
    opposite, render_color_tags, send_to, try_insert, try_remove,
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

/// Hit-chance percentage on `[5, 100]` from attacker hit roll vs
/// target AC. Lower AC = better armor (CircleMUD/D&D semantics —
/// effects modify AC by subtracting for buffs). Tuned so an avg
/// mob (`hit_roll`≈17, ac≈0) effectively never misses; an unrolled
/// stat-zero attacker (`hit_roll`=0) lands 80%; a heavily armored
/// target (ac=10) drops the same swing to ~30%. The 5% floor keeps
/// even a hopeless brawl from being literally unwinnable.
#[must_use]
pub fn hit_chance_pct(hit_roll: i32, target_ac: i32) -> i32 {
    let modifier = hit_roll.saturating_mul(2) - target_ac.saturating_mul(5);
    (80i32.saturating_add(modifier)).clamp(5, 100)
}

/// Posture modifier applied to the target's effective AC at swing
/// time. A non-standing target is easier to hit — they can't
/// dodge as effectively. Each step "down" the posture rank adds
/// 2 to AC (improves attacker odds by ~10%). Sleeping targets
/// auto-hit before this even runs (handled at the call site), so
/// the `Sleeping` arm is included only for symmetry.
#[must_use]
pub fn posture_ac_modifier(p: PostureKind) -> i32 {
    match p {
        PostureKind::Standing => 0,
        PostureKind::Kneeling => 2,
        PostureKind::Sitting => 4,
        PostureKind::Resting => 5,
        PostureKind::Sleeping => 6,
    }
}

/// `Ability.id` for the DODGE skill in the current `fierydev`
/// import. Hardcoded so the swing path doesn't need to scan the
/// catalog by name on every hit. Pinned to 288.
const DODGE_ABILITY_ID: i32 = 288;
/// `Ability.id` for the PARRY skill. Pinned to 287.
const PARRY_ABILITY_ID: i32 = 287;

/// Roll a defender's evasion abilities (Dodge / Parry) against
/// an incoming hit. Returns the name of the ability that evaded
/// (`"dodge"` / `"parry"`) when one fires, or None to let the hit
/// through. Standing-only — a non-standing defender can't reset
/// their stance to evade. Proficiency 0..=1000+; chance is
/// `prof / 50` clipped to 25 (so a fully-mastered Dodge gives a
/// 20% miss-the-swing roll, and a junior 100-prof apprentice
/// dodges 2%).
fn roll_evasion(world: &World, defender: Entity) -> Option<&'static str> {
    if !matches!(
        world.get::<Posture>(defender).map(|p| p.0),
        None | Some(PostureKind::Standing)
    ) {
        return None;
    }
    let known = world.get::<KnownAbilities>(defender)?;
    for (id, kind) in [(DODGE_ABILITY_ID, "dodge"), (PARRY_ABILITY_ID, "parry")] {
        let prof = known
            .entries
            .iter()
            .find(|(aid, _, _)| *aid == id)
            .map_or(0, |(_, p, _)| *p);
        if prof <= 0 {
            continue;
        }
        let chance = (prof / 50).min(25);
        if rand::random_range(0..100) < chance {
            return Some(kind);
        }
    }
    None
}

/// Per-mob memory of the players who've ever swung at them.
/// Lifetime ties to the mob: dies with the mob, never persisted,
/// never serialized. Used by the on-entry aggro path so a mob
/// you fled from re-engages on your return without needing the
/// alignment threshold.
#[derive(Component, Debug, Default)]
pub struct MobMemory(pub std::collections::HashSet<Entity>);

/// Add `attacker` to `mob`'s memory. Inserts the component on
/// first use. No-op if `mob` has been despawned.
pub(crate) fn remember_attacker(world: &mut World, mob: Entity, attacker: Entity) {
    let has = world.get::<MobMemory>(mob).is_some();
    if has {
        if let Some(mut mem) = world.get_mut::<MobMemory>(mob) {
            mem.0.insert(attacker);
        }
    } else {
        let mut set = std::collections::HashSet::new();
        set.insert(attacker);
        try_insert(world, mob, MobMemory(set));
    }
}

/// Pick a random open exit and walk a fleeing mob through it.
/// No-op if the room has no open exits — the swing path falls
/// through and the mob takes the next hit normally. Drops the
/// mob's `Fighting` so attackers will auto-disengage on the room
/// mismatch in the next combat tick.
fn mob_flee(world: &mut World, mob: Entity, from_room: Entity) {
    let candidates: Vec<(mud_db::enums::Direction, Entity)> = world
        .get::<Exits>(from_room)
        .map(|e| {
            e.0.iter()
                .filter_map(|(dir, ed)| {
                    if ed.state == mud_db::enums::ExitState::Open {
                        ed.to.map(|t| (*dir, t))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if candidates.is_empty() {
        return;
    }
    let pick = rand::random_range(0..candidates.len());
    let (dir, target_room) = candidates[pick];
    let mob_name = name_of(world, mob);
    broadcast_room_except_players_rendered(
        world,
        from_room,
        &[mob],
        &format!("{mob_name} panics and flees {}!\r\n", direction_name(dir)),
    );
    try_remove::<Fighting>(world, mob);
    if let Some(mut l) = world.get_mut::<Located>(mob) {
        l.0 = target_room;
    }
    let arrival_dir =
        opposite(dir).map_or("nearby".to_string(), |d| format!("the {}", direction_name(d)));
    broadcast_room_except_players_rendered(
        world,
        target_room,
        &[mob],
        &format!("{mob_name} arrives, panting, from {arrival_dir}.\r\n"),
    );
}

/// One swing's outcome from the d100 roll. Crit and Miss are
/// special cases of the natural-100 / natural-1 corners; the
/// in-between resolves against the computed hit chance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwingOutcome {
    Crit,
    Hit,
    Miss,
}

/// Roll d100 against the computed hit chance. Natural-100 promotes
/// to a critical; everything else resolves normally against the
/// chance band. A 100% `hit_chance` attacker therefore lands 99% as
/// regular hits and 1% as crits — never misses. Sleeping defenders
/// bypass this at the call site (auto-hit).
fn resolve_swing(hit_roll: i32, target_ac: i32) -> SwingOutcome {
    let chance = hit_chance_pct(hit_roll, target_ac);
    let roll = rand::random_range(1..=100);
    if roll == 100 {
        SwingOutcome::Crit
    } else if roll <= chance {
        SwingOutcome::Hit
    } else {
        SwingOutcome::Miss
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

#[allow(clippy::too_many_lines)]
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

    // Mob memory: any swing initiated by a player at a mob lands
    // them in that mob's grudge book, regardless of hit/miss/crit.
    // Re-entering the same room later auto-engages without a
    // fresh aggro check (see `try_engage_remembered_mob`).
    if world.get::<Mob>(s.target).is_some() && world.get::<Player>(s.attacker).is_some() {
        remember_attacker(world, s.target, s.attacker);
    }

    // Hit / miss / crit roll. Sleeping defenders are auto-hit (you
    // can't dodge unconscious) — same special case the existing
    // jolt-awake path already assumed. Otherwise compute the
    // chance from attacker hit_roll vs target AC and roll d100.
    let hit_roll = world.get::<CombatStats>(s.attacker).map_or(0, |cs| cs.hit_roll);
    // Effective AC includes a posture modifier — a sitting /
    // kneeling target is easier to hit than a standing one.
    // Posture modifier is added to AC; lower-AC = harder to hit,
    // so the addition makes the target softer. Sleeping targets
    // bypass the roll entirely (auto-hit) at the call site below.
    let base_ac = world.get::<CombatStats>(s.target).map_or(0, |cs| cs.ac);
    let posture_mod = world
        .get::<Posture>(s.target)
        .map_or(0, |p| posture_ac_modifier(p.0));
    let target_ac = base_ac + posture_mod;
    let outcome = if was_sleeping {
        SwingOutcome::Hit
    } else {
        resolve_swing(hit_roll, target_ac)
    };
    // Active evasion (Dodge / Parry): a defender with the trained
    // skill rolls against a small chance to turn an incoming hit
    // into a miss. Sleeping targets bypass — they can't dodge.
    // Crit-class incoming swings still get rolled — a perfect
    // dodge cancels even a critical hit.
    let evaded_via = if was_sleeping || outcome == SwingOutcome::Miss {
        None
    } else {
        roll_evasion(world, s.target)
    };
    if let Some(via) = evaded_via {
        let attacker_mode = color_mode_for(world, s.attacker);
        let target_mode = color_mode_for(world, s.target);
        send_to(
            world,
            s.attacker,
            render_color_tags(
                &format!("{target_name} {via}s your attack!\r\n"),
                attacker_mode,
            ),
        );
        send_to(
            world,
            s.target,
            render_color_tags(
                &format!("You {via} {}'s attack!\r\n", s.attacker_name),
                target_mode,
            ),
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[s.attacker, s.target],
            &format!(
                "{target_name} {via}s {}'s attack.\r\n",
                s.attacker_name
            ),
        );
        drain_stamina(world, s.attacker, 1);
        return;
    }
    if outcome == SwingOutcome::Miss {
        let attacker_mode = color_mode_for(world, s.attacker);
        let target_mode = color_mode_for(world, s.target);
        send_to(
            world,
            s.attacker,
            render_color_tags(
                &format!("You swing at {target_name} but miss.\r\n"),
                attacker_mode,
            ),
        );
        send_to(
            world,
            s.target,
            render_color_tags(
                &format!("{} swings at you but misses.\r\n", s.attacker_name),
                target_mode,
            ),
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[s.attacker, s.target],
            &format!("{} swings at {target_name} but misses.\r\n", s.attacker_name),
        );
        // Stamina still drains — you swung, you spent the breath.
        drain_stamina(world, s.attacker, 1);
        return;
    }

    // Crit promotes the swing's already-resolved damage by 1.5x.
    // Stacks multiplicatively with the berserk +50% computed in
    // the swing-snapshot phase: a critical berserk swing lands at
    // base * 3/2 * 3/2 = base * 9/4.
    let mut damage = if outcome == SwingOutcome::Crit {
        s.damage.saturating_mul(3) / 2
    } else {
        s.damage
    };
    // Per-swing damage variance: ±25% of the post-crit base, integer
    // floor. Bigger swings get a wider band; sub-4 damage swings
    // pin at variance=0. Floor at 1 so a low roll never zeroes out
    // a swing — that would make the hit/miss roll the only meaningful
    // outcome and the dmg_roll stat decorative.
    let variance_band = damage / 4;
    if variance_band > 0 {
        let delta = rand::random_range(-variance_band..=variance_band);
        damage = damage.saturating_add(delta).max(1);
    }
    let (dead, threshold_msg) = apply_damage(world, s.target, damage);

    // Names may carry XML-Lite tags; render per-recipient so each player
    // gets ANSI or stripped output according to their own COLOR_BLIND flag.
    let attacker_mode = color_mode_for(world, s.attacker);
    let target_mode = color_mode_for(world, s.target);
    let crit_tag = if outcome == SwingOutcome::Crit { " (critical hit!)" } else { "" };
    send_to(
        world,
        s.attacker,
        render_color_tags(
            &format!("You hit {target_name} for {damage} damage{crit_tag}.\r\n"),
            attacker_mode,
        ),
    );
    send_to(
        world,
        s.target,
        render_color_tags(
            &format!(
                "{} hits you for {damage} damage{crit_tag}.\r\n",
                s.attacker_name
            ),
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
    let target_is_mob = world.get::<Mob>(s.target).is_some();
    let wimpy_set = world
        .get::<PlayerFlags>(s.target)
        .is_some_and(|pf| pf.has(mud_db::enums::PlayerFlag::Wimpy));
    let wimpy_pct = world
        .get::<mud_world::WimpyThreshold>(s.target)
        .map_or(25, |w| w.0)
        .clamp(1, 99);
    // Mob auto-flee at <20% HP. Trivial mobs (max HP < 30) can't
    // really flee meaningfully — they'd die on the next swing
    // anyway. Boss-tier mobs (max HP > 200) hold ground; without a
    // proper role flag this absolute-HP heuristic cleanly separates
    // wildlife from set-piece encounters. 50% per-swing roll gives
    // players a window to finish rather than chasing through rooms.
    if target_is_mob
        && let Some(hp) = world.get::<Health>(s.target).copied()
        && hp.hp > 0
        && (30..=200).contains(&hp.max)
        && hp.hp * 5 < hp.max
        && rand::random_range(0..2) == 0
    {
        mob_flee(world, s.target, room);
        return;
    }
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

        // XP loss on death: shave 10% of the player's experience
        // total, floored at 0. Mirrors legacy CircleMUD's
        // round-down-to-bracket-floor behavior loosely; we don't
        // model XP brackets yet, so pure 10% is the right v1.
        // Skipped for level-1 players who have no progress to
        // lose.
        let xp_lost = world
            .get::<mud_world::Profile>(victim)
            .filter(|p| p.level > 1)
            .map_or(0, |p| p.experience / 10);
        if xp_lost > 0 {
            if let Some(mut prof) = world.get_mut::<mud_world::Profile>(victim) {
                prof.experience = (prof.experience - xp_lost).max(0);
            }
            send_to(
                world,
                victim,
                format!("You feel the weight of death — {xp_lost} experience drains away.\r\n"),
            );
        }

        // PvP alignment shift: if a Player killed this Player,
        // the killer's alignment slides 50 points toward evil
        // (clamped at -1000). PvP carries weight even for
        // self-styled "neutral" players. The killer is whoever
        // had Fighting(victim) at death-time and is themselves
        // a Player.
        let pvp_killer: Option<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Fighting), With<Player>>();
            q.iter(world).find(|(e, f)| f.0 == victim && *e != victim).map(|(e, _)| e)
        };
        if let Some(killer) = pvp_killer
            && let Some(mut cs) = world.get_mut::<CombatStats>(killer)
        {
            let before = cs.alignment;
            cs.alignment = (cs.alignment - 50).max(-1000);
            let after = cs.alignment;
            if before != after {
                send_to(
                    world,
                    killer,
                    "A shadow falls across your soul as you take a player's life.\r\n",
                );
            }
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
            // Loot-claim window: 5 minutes for the killer.
            // Player-only — mob killers don't claim corpses (this
            // path is reached only when a player landed the
            // killing blow against another mob; the killer
            // lookup above filters to Player entities).
            if let Some(k) = killer {
                world.get_entity_mut(corpse).unwrap().insert(mud_world::LootClaim {
                    owner: k,
                    expires_at: std::time::Instant::now()
                        + std::time::Duration::from_secs(300),
                });
            }
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
                    // hit_roll high enough to guarantee a 100% hit
                    // chance (see hit_chance_pct's clamp). Tests
                    // shouldn't gamble on the miss path.
                    hit_roll: 10,
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
        // Damage = 7 ± (7/4 = 1) for normal, or 10 ± (10/4 = 2)
        // on the 1% crit branch. So hp lands in [50-12 .. 50-6],
        // i.e. 38..=44. Anything else means the swing didn't
        // connect at all (impossible at hit_chance=100%).
        assert!(
            (38..=44).contains(&hp.hp),
            "target HP within damage+crit+variance band, got {}",
            hp.hp,
        );
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
    fn hit_chance_curve() {
        // Stat-zero matchup: 80% baseline.
        assert_eq!(hit_chance_pct(0, 0), 80);
        // Each point of hit_roll = +2%.
        assert_eq!(hit_chance_pct(5, 0), 90);
        assert_eq!(hit_chance_pct(10, 0), 100);
        // Each point of AC = -5% (lower AC = better defense).
        assert_eq!(hit_chance_pct(0, 10), 30);
        assert_eq!(hit_chance_pct(0, 20), 5);
        // Floor and ceiling clamp.
        assert_eq!(hit_chance_pct(0, 100), 5);
        assert_eq!(hit_chance_pct(100, 0), 100);
        // Mixed: avg mob (hit_roll=17) vs avg defender (ac=0) is
        // capped at 100% — strong attackers should never miss
        // unarmored targets.
        assert_eq!(hit_chance_pct(17, 0), 100);
        // Heavy armor flips it: same attacker vs ac=10.
        assert_eq!(hit_chance_pct(17, 10), 64);
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
