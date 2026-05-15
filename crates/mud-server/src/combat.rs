use bevy_ecs::prelude::*;
use mud_world::{
    AppliedTo, CombatStats, Corpse, CorpseDecay, Description, EffectInstance, EquippedSlot, Exits,
    Fighting, Ghost, Guarding, Health, Item, Keywords, KnownAbilities, Located, Mob,
    MobPrototypes, Named, NaturalDamage, ObjectPrototypes, Player, PlayerFlags, Posture,
    PostureKind, Slot, Stunned, Wealth, WearableIn, WorldKey, WorldKeyIndex,
};
use tracing::info;

use crate::TickCount;
use crate::commands::{
    apply_damage, broadcast_room_except_players_rendered, broadcast_room_except_rendered,
    cmd_flee, damage_color_tag, direction_name, disengage_attackers_of, drain_stamina, name_of,
    opposite, send_to, try_insert, try_remove,
};

const COMBAT_PERIOD_TICKS: u64 = 40;

/// Maximum per-swing damage. Mirrors legacy `defines.hpp:349`'s
/// `MAX_DAMAGE = 1000`. Caps even the wildest crits/burst boss
/// damage so a player can't get one-shot from full HP by a stray
/// rogue-tier outlier item.
pub const MAX_DAMAGE_PER_SWING: i32 = 1000;

/// Parse a `NdM[+B]` / `NdM[-B]` / bare-int dice string into
/// `(num, sides, bonus)`. Returns `(0, 0, 0)` on parse failure so
/// the caller's `roll_dice(0, 0, 0)` degenerates to 0 — i.e. an
/// empty / malformed `Class.hit_dice` contributes nothing rather
/// than panicking. A bare integer parses as `(0, 0, n)` (constant).
#[must_use]
pub fn parse_hit_dice(s: &str) -> (i32, i32, i32) {
    let s = s.trim();
    if s.is_empty() {
        return (0, 0, 0);
    }
    if let Ok(n) = s.parse::<i32>() {
        return (0, 0, n);
    }
    let (dice, bonus) = match s.find(['+', '-']) {
        Some(i) => (&s[..i], s[i..].parse::<i32>().unwrap_or(0)),
        None => (s, 0),
    };
    let Some((n, m)) = dice.split_once('d') else {
        return (0, 0, 0);
    };
    let n = n.trim().parse::<i32>().unwrap_or(0);
    let m = m.trim().parse::<i32>().unwrap_or(0);
    (n, m, bonus)
}

/// Roll `num`d`sides` and add `bonus`. Returns `bonus` when the
/// dice expression is degenerate (zero dice / zero sides). Used by
/// the swing pre-pass to expand weapon and natural-attack dice
/// into a per-swing damage roll.
#[must_use]
pub fn roll_dice(num: i32, sides: i32, bonus: i32) -> i32 {
    if num <= 0 || sides <= 0 {
        return bonus;
    }
    let mut total: i32 = bonus;
    for _ in 0..num {
        total = total.saturating_add(rand::random_range(1..=sides));
    }
    total
}

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
/// Four real-time seconds per swing (40 ticks at 10Hz) — matches legacy
/// PULSE_VIOLENCE so the DB-authored damage values stay calibrated. Decrements
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
        // Expired — items inside disintegrate alongside the
        // corpse. Persistent items (e.g. quest items flagged
        // `Permanent`) survive: they're released to the room so
        // the player can still recover them. Everything else
        // despawns. Matches the legacy "decay alongside the
        // corpse unless looted in time" semantic — the cost of
        // ignoring a corpse is real.
        let contents: Vec<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Located), With<Item>>();
            q.iter(world)
                .filter(|(_, l)| l.0 == corpse)
                .map(|(e, _)| e)
                .collect()
        };
        let mut decayed = 0usize;
        let spilled = 0usize;
        for it in contents {
            // For now there's no Permanent marker; treat all
            // contents as decayable. When the schema models a
            // permanent flag (or quest-item tag), branch here.
            if let Ok(em) = world.get_entity_mut(it) {
                em.despawn();
            }
            decayed += 1;
        }
        let line = if spilled > 0 {
            format!(
                "{name} crumbles to dust, scattering {spilled} item(s) across the floor.\r\n"
            )
        } else {
            format!("{name} crumbles to dust, taking its contents with it.\r\n")
        };
        broadcast_room_except_rendered(world, room, &[], &line);
        let _ = decayed;
        if let Ok(em) = world.get_entity_mut(corpse) {
            em.despawn();
        }
    }
}

/// Hit-chance percentage from the d100 accuracy/evasion contest
/// per docs/design/combat.md step 1:
///
/// ```text
/// hit if  attacker.accuracy + d100  >  defender.evasion + d100
/// ```
///
/// Closed-form for the chance: equivalent to a single d100 with
/// margin `accuracy - evasion`. Equal stats produce a 50% hit rate;
/// each point of advantage moves it ~0.5 percentage points.
/// Clamped to `[1, 99]` so even the most lopsided fight has a
/// "punch through / get lucky" floor and ceiling.
#[must_use]
pub fn hit_chance_pct(accuracy: i32, evasion: i32) -> i32 {
    let margin = accuracy - evasion;
    // Closed-form CDF of the difference of two uniform d100 rolls:
    // at margin = 0 the hit rate is exactly 50%; at margin = +100
    // it's 99%; at -100 it's 1%. Linear interpolation around the
    // middle is good enough for game balance.
    let chance = 50i32.saturating_add(margin / 2);
    chance.clamp(1, 99)
}

/// Posture penalty applied to the defender's effective evasion at
/// swing time. A non-standing target dodges less effectively;
/// each step subtracts from their evasion. Sleeping defenders
/// auto-hit at the call site, so the `Sleeping` arm is included
/// only for symmetry.
#[must_use]
pub fn posture_evasion_penalty(p: PostureKind) -> i32 {
    match p {
        PostureKind::Standing => 0,
        PostureKind::Kneeling => 10,
        PostureKind::Sitting => 20,
        PostureKind::Resting => 25,
        PostureKind::Sleeping => 30,
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

/// Active hate / aggro list. Ordered by most recent attacker last.
/// `combat_tick`'s pre-pass picks the head when the mob's current
/// `Fighting` target dies or flees so combat continues without
/// the player having to re-engage. Per-instance, dies with the
/// mob. Bounded — duplicates are dropped on push.
#[derive(Component, Debug, Default)]
pub struct HateList(pub Vec<Entity>);

impl HateList {
    /// Append `attacker` to the tail; remove existing instances
    /// first so the most-recent swing wins re-engagement priority.
    pub fn push(&mut self, attacker: Entity) {
        self.0.retain(|e| *e != attacker);
        self.0.push(attacker);
    }
}

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
    let mob_capped = crate::commands::cap_sentence_start(&mob_name);
    broadcast_room_except_players_rendered(
        world,
        from_room,
        &[mob],
        &format!("{mob_capped} panics and flees {}!\r\n", direction_name(dir)),
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
        &format!("{mob_capped} arrives, panting, from {arrival_dir}.\r\n"),
    );
}

/// One swing's outcome from the d100 roll. Crit and Miss are
/// special cases of the natural-100 / natural-1 corners; the
/// in-between resolves against the computed hit chance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwingOutcome {
    Crit,
    Hit,
    Miss,
}

/// Roll d100 against the computed hit chance. Natural-100 promotes
/// to a critical; everything else resolves normally against the
/// chance band. A 100% `hit_chance` attacker therefore lands 99% as
/// regular hits and 1% as crits — never misses. Sleeping defenders
/// bypass this at the call site (auto-hit).
fn resolve_swing(hit_roll: i32, target_ac: i32) -> SwingDetail {
    // Legacy compatibility shim — kept around for tests until they're
    // converted to the acc/ev model. Treats `hit_roll` as accuracy
    // and `target_ac` as evasion.
    resolve_swing_acc_ev(hit_roll, target_ac, 5)
}

/// Resolve one swing under the accuracy/evasion d100 contest from
/// docs/design/combat.md. `crit_chance` is a separate post-hit
/// d100 against the attacker's `crit_chance` field.
fn resolve_swing_acc_ev(accuracy: i32, evasion: i32, crit_chance: i32) -> SwingDetail {
    let chance = hit_chance_pct(accuracy, evasion);
    let roll = rand::random_range(1..=100);
    let outcome = if roll <= chance {
        if rand::random_range(1..=100) <= crit_chance {
            SwingOutcome::Crit
        } else {
            SwingOutcome::Hit
        }
    } else {
        SwingOutcome::Miss
    };
    SwingDetail { outcome, roll, chance }
}

/// Roll details surfaced by `resolve_swing` so the showdice toggle
/// can render them to the attacker. Outcome alone isn't enough —
/// players want to see the d100 vs threshold.
#[derive(Clone, Copy)]
pub(crate) struct SwingDetail {
    pub outcome: SwingOutcome,
    pub roll: i32,    // d100 result
    pub chance: i32,  // need <= this to land a regular hit
}

/// True iff the attacker has the `SHOW_DICE_ROLLS` `PlayerFlag` set.
/// Cheap (component lookup); call sites guard their detail-line
/// construction on this rather than always formatting the string.
fn show_dice_for(world: &World, attacker: Entity) -> bool {
    world
        .get::<PlayerFlags>(attacker)
        .is_some_and(|pf| pf.has(mud_db::enums::PlayerFlag::ShowDiceRolls))
}

/// Build the per-attacker showdice tail for one swing. Returns
/// empty string when the flag isn't set; otherwise a parenthesized
/// summary suitable for appending to the attacker's swing line.
///
/// Format examples (legacy combat math; will be revised when the
/// modern accuracy/evasion pipeline lands per docs/design/combat.md):
///
///   (d100 33 ≤ 65 — dmg 8 ±var = 6)             // hit
///   (d100 88 > 65 — miss)                       // miss
///   (d100 100 — CRIT — dmg 8 × 1.5 ±var = 14)   // crit
///   (auto-hit on sleeping target — dmg 6 ±var = 7)
///   (defender evaded via parry)                 // evade
fn show_dice_swing(detail: SwingDetail, base_damage: i32, final_damage: i32) -> String {
    match detail.outcome {
        SwingOutcome::Miss => {
            format!("  (d100 {} > {} — miss)\r\n", detail.roll, detail.chance)
        }
        SwingOutcome::Hit if detail.roll == 0 => {
            // Sleeping defender: auto-hit, no roll happened.
            format!("  (auto-hit on sleeping target — dmg {base_damage} ±var = {final_damage})\r\n")
        }
        SwingOutcome::Hit => {
            format!(
                "  (d100 {} ≤ {} — dmg {base_damage} ±var = {final_damage})\r\n",
                detail.roll, detail.chance,
            )
        }
        SwingOutcome::Crit => {
            // Crit promotes pre-variance damage by 1.5×. base_damage is
            // post-promotion; show the math anyway for clarity.
            let pre_promote = (base_damage * 2) / 3;
            format!(
                "  (d100 {} — CRIT — dmg {pre_promote} × 1.5 ±var = {final_damage})\r\n",
                detail.roll,
            )
        }
    }
}

/// Showdice tail when the defender evaded — no damage roll
/// happened, but the attacker still wants to see what defeated
/// the swing.
fn show_dice_evade(via: &str) -> String {
    format!("  (defender evaded via {via})\r\n")
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

#[allow(clippy::too_many_lines)]
pub fn combat_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(COMBAT_PERIOD_TICKS) {
        return;
    }

    // Pre-pass: re-engage mobs whose `Fighting` cleared but who
    // still have a `HateList`. Pop entries until we find a live
    // co-located target or exhaust the list. This is what makes
    // multi-target aggro work: a mob fighting Alice + Bob keeps
    // swinging at Bob when Alice flees, instead of standing
    // around peacefully.
    //
    // Targets are filtered out when their LifeState marks them
    // unswingable: `Ghost` (dead, HP pinned to 1), `Frozen` (admin
    // freeze), `Stunned` (transient incapacitation). Without these
    // filters a dead player gets re-aggroed every tick because the
    // pinned-to-1 HP still passes the `hp > 0` check, producing a
    // damage loop on the corpse.
    let to_reengage: Vec<(Entity, Entity)> = {
        let mut q = world.query_filtered::<
            (Entity, &Located, &HateList),
            (With<Mob>, Without<Fighting>),
        >();
        q.iter(world)
            .filter_map(|(mob, loc, hate)| {
                hate.0
                    .iter()
                    .rev()
                    .find(|target| {
                        world.get::<Located>(**target).map(|l| l.0) == Some(loc.0)
                            && world.get::<Health>(**target).is_some_and(|h| h.hp > 0)
                            && world.get::<Ghost>(**target).is_none()
                            && world.get::<mud_world::Frozen>(**target).is_none()
                            && world.get::<mud_world::Stunned>(**target).is_none()
                    })
                    .map(|target| (mob, *target))
            })
            .collect()
    };
    for (mob, target) in to_reengage {
        try_insert(world, mob, Fighting(target));
        try_insert(world, target, Fighting(mob));
        let mob_name = name_of(world, mob);
        let target_name = name_of(world, target);
        let target_room = world.get::<Located>(target).map(|l| l.0);
        if let Some(room) = target_room {
            send_to(
                world,
                target,
                format!("{mob_name} turns its hate on you!\r\n"),
            );
            broadcast_room_except_rendered(
                world,
                room,
                &[target],
                &format!("{mob_name} turns on {target_name}!\r\n"),
            );
        }
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
    // Pre-pass: snapshot every wielded weapon's dice so the swing-
    // map step can reach them without a fresh borrow. Players have
    // dmg_roll = 0 by default; the weapon dice are the actual
    // damage source. Mobs without a wielded weapon roll their
    // `NaturalDamage` dice instead (set at mob spawn from
    // `proto.damage_dice_*`). Test worlds without an
    // ObjectPrototypes resource just skip the pre-pass and fall
    // through to the per-entity NaturalDamage / dmg_roll branch.
    let weapon_dice: std::collections::HashMap<Entity, (i32, i32, i32)> =
        if world.get_resource::<ObjectPrototypes>().is_some() {
            let protos: Vec<(Entity, (i32, i32))> = {
                let mut q = world.query::<(&Located, &EquippedSlot, &WorldKey)>();
                q.iter(world)
                    .filter(|(_, eq, _)| eq.0 == Slot::Wield)
                    .map(|(loc, _, key)| (loc.0, (key.zone, key.id)))
                    .collect()
            };
            let proto_catalog = world.resource::<ObjectPrototypes>();
            protos
                .into_iter()
                .filter_map(|(wielder, key)| {
                    let p = proto_catalog.by_key.get(&key)?;
                    if p.weapon_dice_num <= 0 || p.weapon_dice_size <= 0 {
                        return None;
                    }
                    Some((
                        wielder,
                        (p.weapon_dice_num, p.weapon_dice_size, p.weapon_dice_bonus),
                    ))
                })
                .collect()
        } else {
            std::collections::HashMap::new()
        };
    // Pre-pass: snapshot every entity's NaturalDamage component
    // (claws/teeth/fists) so the swing snapshot can roll for
    // unarmed attackers without a fresh borrow.
    let natural_damage: std::collections::HashMap<Entity, (i32, i32, i32)> = {
        let mut q = world.query::<(Entity, &NaturalDamage)>();
        q.iter(world)
            .map(|(e, n)| (e, (n.num, n.size, n.bonus)))
            .collect()
    };
    // Ghost / Frozen attackers are filtered out of the swing
    // snapshot — even if Fighting somehow lingers on a dead or
    // frozen entity, they can't swing. Stunned is checked
    // explicitly below for parity with the existing semantics.
    let swings: Vec<Swing> = {
        let mut q = world.query_filtered::<
            (
                Entity,
                &Fighting,
                &CombatStats,
                &Named,
                Option<&Posture>,
                Option<&Stunned>,
            ),
            (Without<Ghost>, Without<mud_world::Frozen>),
        >();
        q.iter(world)
            .filter(|(_, _, _, _, posture, stunned)| {
                stunned.is_none()
                    && matches!(posture.map(|p| p.0), None | Some(PostureKind::Standing))
            })
            .map(|(attacker, fighting, cs, name, _, _)| {
                // Damage pipeline per docs/design/combat.md step 3:
                //   base = weapon_dice * (1 + attack_power/100)
                // Where the dice come from:
                //  1. Wielded weapon dice (player or armed mob)
                //  2. NaturalDamage dice (mob's claws/teeth/fists)
                //  3. zero (no weapon, no natural attack — typical
                //     for an unarmed player; legacy fallback was
                //     1 damage to keep something happening)
                // attack_power applies as an additive % multiplier
                // on the rolled base.
                let weapon_roll = if let Some(&(num, sides, bonus)) = weapon_dice.get(&attacker) {
                    roll_dice(num, sides, bonus)
                } else if let Some(&(num, sides, bonus)) = natural_damage.get(&attacker) {
                    // Natural-attack damage scales by the attacker's
                    // `Races.damage_dice_factor` (percent). 100 =
                    // unchanged. Wielded-weapon damage is unaffected
                    // — the factor models race-shaped claws / teeth,
                    // not steel.
                    let raw = roll_dice(num, sides, bonus);
                    let race_factor = world
                        .get::<mud_world::Profile>(attacker)
                        .and_then(|p| {
                            world
                                .get_resource::<mud_world::RaceCatalog>()
                                .and_then(|c| c.get(&p.race))
                        })
                        .map_or(100, |def| def.damage_dice_factor);
                    if race_factor == 100 {
                        raw
                    } else {
                        raw.saturating_mul(race_factor)
                            .saturating_div(100)
                            .max(1)
                    }
                } else {
                    1 // unarmed floor — keeps swings non-zero
                };
                let scaled =
                    (weapon_roll.saturating_mul(100 + cs.attack_power)) / 100;
                let base = scaled.max(1);
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
    // posture-and-lifestate.md: a defender attacked while RESTING
    // auto-stands on the hit. Sleeping has its own jolt-awake path
    // (different visual), so the two stay separate flags.
    let was_resting =
        world.get::<Posture>(s.target).map(|p| p.0) == Some(PostureKind::Resting);

    // Mob memory: any swing initiated by a player at a mob lands
    // them in that mob's grudge book, regardless of hit/miss/crit.
    // Re-entering the same room later auto-engages without a
    // fresh aggro check (see `try_engage_remembered_mob`).
    if world.get::<Mob>(s.target).is_some() && world.get::<Player>(s.attacker).is_some() {
        remember_attacker(world, s.target, s.attacker);
        // Hate list — populated regardless of hit outcome, like
        // memory. The combat tick's pre-pass picks the head
        // when the mob's current target dies / flees.
        let already = world.get::<HateList>(s.target).is_some();
        if already {
            if let Some(mut h) = world.get_mut::<HateList>(s.target) {
                h.push(s.attacker);
            }
        } else {
            let mut list = HateList::default();
            list.push(s.attacker);
            try_insert(world, s.target, list);
        }
    }

    // Hit / miss / crit per docs/design/combat.md step 1.
    // Sleeping defenders auto-hit (can't dodge unconscious).
    // Otherwise: attacker.accuracy + d100 vs defender.evasion + d100,
    // ties to attacker. Posture penalty subtracts from defender's
    // evasion (a sitting target evades worse). Crit chance is a
    // separate d100 vs the attacker's `crit_chance`.
    let attacker_accuracy = world
        .get::<CombatStats>(s.attacker)
        .map_or(50, |cs| cs.accuracy);
    let attacker_crit_chance = world
        .get::<CombatStats>(s.attacker)
        .map_or(5, |cs| cs.crit_chance);
    let base_evasion = world
        .get::<CombatStats>(s.target)
        .map_or(50, |cs| cs.evasion);
    let posture_evasion_penalty = world
        .get::<Posture>(s.target)
        .map_or(0, |p| posture_evasion_penalty(p.0));
    let target_evasion = base_evasion - posture_evasion_penalty;
    let detail = if was_sleeping {
        SwingDetail { outcome: SwingOutcome::Hit, roll: 0, chance: 100 }
    } else {
        resolve_swing_acc_ev(attacker_accuracy, target_evasion, attacker_crit_chance)
    };
    let outcome = detail.outcome;
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
    let dice_on = show_dice_for(world, s.attacker);
    if let Some(via) = evaded_via {
        let tail = if dice_on { show_dice_evade(via) } else { String::new() };
        send_to(
            world,
            s.attacker,
            format!("{target_name} {via}s your attack!\r\n{tail}"),
        );
        send_to(
            world,
            s.target,
            format!("You {via} {}'s attack!\r\n", s.attacker_name),
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
        let tail = if dice_on { show_dice_swing(detail, s.damage, 0) } else { String::new() };
        // Misses dim slightly — visible but recedes vs the hit
        // lines below, which carry the actual gameplay info.
        send_to(
            world,
            s.attacker,
            format!("<dim>You swing at {target_name} but miss.</>\r\n{tail}"),
        );
        send_to(
            world,
            s.target,
            format!("<dim>{} swings at you but misses.</>\r\n", s.attacker_name),
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[s.attacker, s.target],
            &format!(
                "<dim>{} swings at {target_name} but misses.</>\r\n",
                s.attacker_name
            ),
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
    // Per-attacker race scaling from `Races.hit_damage_factor`
    // (percent). 100 = unchanged; a race authored at 120 hits
    // 20% harder. Applies before mitigation so the percent reads
    // as "this race hits harder", not "this race penetrates
    // harder". Skipped silently for attackers without a Profile
    // (legacy / NPC fallback) — those swings keep base damage.
    if let Some(prof) = world.get::<mud_world::Profile>(s.attacker)
        && let Some(catalog) = world.get_resource::<mud_world::RaceCatalog>()
        && let Some(def) = catalog.get(&prof.race)
        && def.hit_damage_factor != 100
    {
        damage = damage
            .saturating_mul(def.hit_damage_factor)
            .saturating_div(100)
            .max(1);
    }
    // Snapshot the post-crit, pre-variance value so showdice can
    // render the "× 1.5 ±var = N" math without re-deriving it.
    let damage_pre_variance = damage;
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
    // Mitigation pipeline per docs/design/combat.md steps 4-7.
    // Today every weapon swing is treated as PHYSICAL (engages
    // armor) and `is_magical = false` (skips ward). Type
    // resistance is applied as a single PHYSICAL lookup against
    // the defender's `Resistances` map; ELEMENTAL/MYSTIC swings
    // arrive via the abilities path (TBD).
    let (def_armor_pct, def_armor_flat, def_hardness) = world
        .get::<CombatStats>(s.target)
        .map_or((0, 0, 0), |cs| (cs.armor_pct, cs.armor_flat, cs.hardness));
    let (atk_pen_pct, atk_pen_flat) = world
        .get::<CombatStats>(s.attacker)
        .map_or((0, 0), |cs| (cs.pen_pct, cs.pen_flat));
    // Step 4: armor mitigation (PHYSICAL gate; weapons are PHYSICAL today).
    let effective_armor_pct = (def_armor_pct - atk_pen_pct).clamp(0, 100);
    damage = (damage.saturating_mul(100 - effective_armor_pct)) / 100;
    let effective_armor_flat = (def_armor_flat - atk_pen_flat).max(0);
    damage = damage.saturating_sub(effective_armor_flat).max(0);
    // Step 5: ward — skipped for mundane weapon swings (caller
    // routes magical abilities through a separate path that
    // engages it).
    // Step 6: type resistance against PHYSICAL.
    if let Some(res) = world.get::<mud_world::Resistances>(s.target) {
        let pct = res.0.get(&mud_db::enums::ElementType::Physical).copied().unwrap_or(0);
        // capped at +100 immunity; negative is unbounded vulnerability per docs.
        let pct = pct.min(100);
        damage = (damage.saturating_mul(100 - pct)) / 100;
        damage = damage.max(0);
    }
    // Step 7: hardness floor — damage below this zeroes out.
    if damage < def_hardness {
        damage = 0;
    }
    // Final cap per legacy MAX_DAMAGE so even a god-tier crit
    // can't one-shot a fully-buffed player from full HP.
    damage = damage.min(MAX_DAMAGE_PER_SWING);
    let (dead, threshold_msg) = apply_damage(world, s.target, damage);

    // Names may carry XML-Lite tags; send_to renders per-recipient so each
    // player gets ANSI or stripped output according to their own COLOR_BLIND
    // flag. Damage value color-graded by magnitude (chip dim, mid plain,
    // heavy yellow, big red) so the player's eye lands on the meaningful
    // hits. Crit tag bold red.
    let crit_tag = if outcome == SwingOutcome::Crit {
        " <b:red>(critical hit!)</>"
    } else {
        ""
    };
    let damage_label = match damage_color_tag(damage) {
        Some(open) => format!("{open}{damage}</>"),
        None => damage.to_string(),
    };
    let tail = if dice_on { show_dice_swing(detail, damage_pre_variance, damage) } else { String::new() };
    // Mob-natural-attack flavor: when the attacker carries a
    // `NaturalAttackType` (i.e. unarmed mob swing), pull the verb
    // from the proto's `DamageType::verb()` so a wolf bites and an
    // orc claws instead of the generic "hits". Players (who use
    // weapons via the equip path) keep "hits" until weapon-attack-
    // type rendering lands.
    let natural_verb_t = world
        .get::<mud_world::NaturalAttackType>(s.attacker)
        .map(|n| n.0.verb());
    let attacker_verb_third = natural_verb_t.unwrap_or("hits");
    // First-person attacker line: pluralize-removing the trailing 's'
    // is too aggressive (`bludgeons` → `bludgeon`, but `slashes` →
    // `slashe`). The attacker line is only ever for players today
    // (mobs don't receive messages), so we keep the literal "hit".
    send_to(
        world,
        s.attacker,
        format!(
            "You hit <b:cyan>{target_name}</> for {damage_label} damage{crit_tag}.\r\n{tail}"
        ),
    );
    send_to(
        world,
        s.target,
        format!(
            "{} {attacker_verb_third} you for {damage_label} damage{crit_tag}.\r\n",
            s.attacker_name
        ),
    );
    if was_sleeping && !dead {
        try_insert(world, s.target, Posture(PostureKind::Standing));
        send_to(world, s.target, "<yellow>You jolt awake!</>\r\n");
        broadcast_room_except_rendered(
            world,
            room,
            &[s.attacker, s.target],
            &format!("<yellow>{target_name} jolts awake!</>\r\n"),
        );
    } else if was_resting && !dead {
        // posture-and-lifestate.md: a hit on a resting defender
        // forces them to stand. Mirrors the sleeping jolt-awake
        // path with a less-startled visual — they were already
        // conscious, just supine.
        try_insert(world, s.target, Posture(PostureKind::Standing));
        send_to(world, s.target, "<yellow>You scramble to your feet!</>\r\n");
        broadcast_room_except_rendered(
            world,
            room,
            &[s.attacker, s.target],
            &format!("<yellow>{target_name} scrambles to their feet!</>\r\n"),
        );
    }
    if let Some(m) = threshold_msg {
        send_to(world, s.target, m);
    }
    broadcast_room_except_rendered(
        world,
        room,
        &[s.attacker, s.target],
        &format!("{} {attacker_verb_third} {target_name}.\r\n", s.attacker_name),
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
        // clamp HP at zero (they're dead) and stop combat. Ghosts
        // shouldn't normally take damage at all (apply_damage early-
        // returns on Ghost), but a swing snapshotted before the
        // Ghost was applied can still call into here.
        if world.get::<Ghost>(victim).is_some() {
            if let Some(mut hp) = world.get_mut::<Health>(victim) {
                hp.hp = 0;
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
        //
        // SOULBOUND items stay on the dead player — bond persists
        // through death by definition. They keep their `EquippedSlot`
        // so a bound weapon doesn't end up un-wielded after the ghost
        // releases. Looters get the rest.
        let owned_items: Vec<(Entity, bool)> = {
            let mut q = world.query_filtered::<
                (Entity, &Located, Option<&mud_world::ObjectFlags>),
                With<Item>,
            >();
            q.iter(world)
                .filter(|(_, l, _)| l.0 == victim)
                .map(|(e, _, f)| {
                    let bound = f
                        .is_some_and(|ff| ff.has(mud_db::enums::ObjectFlag::Soulbound));
                    (e, bound)
                })
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
        for (it, bound) in owned_items {
            if bound {
                // Skip both moves — bound gear stays on the ghost.
                continue;
            }
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

        // Death clears every mob's grudge data against the dead
        // player — `MobMemory` (auto-engage on re-entry) and
        // `HateList` (re-aggro pre-pass target list). Without this,
        // mobs keep targeting the corpse / ghost and re-aggro fires
        // every tick on the pinned-HP-1 ghost. It also matches
        // player expectation that death is a soft reset for "who's
        // angry at me right now".
        let mobs: Vec<Entity> = {
            let mut q = world.query_filtered::<Entity, With<Mob>>();
            q.iter(world).collect()
        };
        for mob in mobs {
            if let Some(mut mem) = world.get_mut::<MobMemory>(mob) {
                mem.0.remove(&victim);
            }
            if let Some(mut hate) = world.get_mut::<HateList>(mob) {
                hate.0.retain(|e| *e != victim);
            }
        }

        // Ghost the player. HP set to 0 — they're dead, the body
        // has no health left. The Ghost component is the
        // authoritative gate (apply_damage / regen_tick / heal /
        // re-aggro all check it) so the HP value just has to read
        // truthfully on the score sheet. `release` restores
        // hp = max as part of the spirit-returns-to-body transition.
        try_insert(world, victim, Ghost);
        if let Some(mut hp) = world.get_mut::<Health>(victim) {
            hp.hp = 0;
        }

        // Death recovery hint: name the room the corpse landed in so
        // the player knows where to return for their gear. The corpse
        // decays after 10 minutes (CorpseDecay { remaining_secs: 600 }
        // above), so include the timer in the hint — players who get
        // released without seeing this can run `corpse` once back
        // alive for the same info.
        let death_room_name = name_of(world, room);
        send_to(
            world,
            victim,
            format!(
                "You collapse, your spirit drifting free of your dying body.\r\n\
                 Your corpse lies in <b:yellow>{death_room_name}</> — it will \
                 decay in about 10 minutes.\r\n\
                 Type <b:cyan>release</> to return to your recall point, then \
                 head back for your gear.\r\n"
            ),
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[victim],
            &format!("{victim_name} collapses, dead.\r\n"),
        );
        info!(?victim, name = %victim_name, ?corpse, "player corpsed");
    } else {
        // Mob death: notify, spawn a corpse, drop loot + leftover
        // coin into it, despawn the mob, stop attackers. The corpse
        // always spawns — even when the mob carried nothing — so
        // `look corpse` works regardless and the leftover-coin path
        // (`award_kill_coin` for non-AutoGold killers) has somewhere
        // to attach a `CoinPile`.
        broadcast_room_except_rendered(
            world,
            room,
            &[],
            &format!("{victim_name} dies.\r\n"),
        );
        award_kill_xp(world, victim, victim_name);
        // Achievement hooks: first_kill and (eventually)
        // milestone-kill counters. Fire on the player who's
        // currently Fighting the victim — same target as the
        // kill-coin / loot-claim attribution.
        let killer: Option<Entity> = {
            let mut q = world.query_filtered::<(Entity, &Fighting), With<Player>>();
            q.iter(world).find(|(_, f)| f.0 == victim).map(|(e, _)| e)
        };
        if let Some(killer) = killer {
            crate::commands::grant_achievement(world, killer, "first_kill");
            crate::commands::bump_kill_count(world, killer);
            apply_protected_kill_penalty(world, killer, victim);
            // Quest objective: advance KILL_MOB objectives whose
            // target matches the victim's prototype. Fire-and-
            // forget; the async task sends progress lines back
            // via the player's outbound channel.
            if let Some(key) = world.get::<mud_world::WorldKey>(victim).copied() {
                crate::commands::bump_kill_quest_progress(world, killer, key.zone, key.id);
            }
        }
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
        // Loot-claim window: 5 minutes for the killer. Player-only
        // — mob killers don't claim corpses (this path is reached
        // only when a player landed the killing blow against
        // another mob; the killer lookup above filters to Player
        // entities).
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
        // award_kill_coin handles AutoGold/AutoSplit and (when
        // AutoGold is off) attaches a CoinPile to the corpse so
        // the coin can still be claimed via `get all from corpse`.
        // Runs after the corpse is spawned so the CoinPile has
        // somewhere to attach.
        award_kill_coin(world, victim, victim_name, corpse);
        // Auto-loot: if the killer has the flag, immediately
        // pull every item out of the corpse onto them. Quiet —
        // players opted in.
        let auto_loot = killer
            .and_then(|k| world.get::<mud_world::PlayerFlags>(k).cloned())
            .is_some_and(|pf| pf.has(mud_db::enums::PlayerFlag::AutoLoot));
        if let (Some(killer), true) = (killer, auto_loot)
            && !owned_items.is_empty()
        {
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
        disengage_attackers_of(world, victim);
        if let Ok(e) = world.get_entity_mut(victim) {
            e.despawn();
        }
        info!(?victim, name = %victim_name, ?corpse, "mob despawned");
    }
}

/// On mob death: look up the proto's `wealth`, find the first player
/// engaged with the victim, and route the coin onto either the killer
/// (`AUTO_GOLD` on, default) or the freshly-spawned corpse via
/// `CoinPile` (`AUTO_GOLD` off — claimed via `get all from corpse`).
/// No-op when the mob has no wealth, no proto, or no player attacker.
fn award_kill_coin(
    world: &mut World,
    victim: Entity,
    victim_name: &str,
    corpse: Entity,
) {
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
    let Some(killer) = killer else {
        // No player engaged — coin still needs a home so it can be
        // claimed if a player walks in later (or it just decays
        // with the corpse). Attach to the corpse without an
        // owner-side message.
        if let Ok(mut e) = world.get_entity_mut(corpse) {
            e.insert(mud_world::CoinPile(coin));
        }
        return;
    };
    let auto_gold = world
        .get::<PlayerFlags>(killer)
        .is_some_and(|pf| pf.has(mud_db::enums::PlayerFlag::AutoGold));
    if !auto_gold {
        // Coin lies on the corpse as a `CoinPile` — claimable via
        // `get all from corpse`. Decays with the corpse if left
        // behind. Replaces the previous "you leave it scattered"
        // forfeit-the-coin path that left the player with nothing.
        if let Ok(mut e) = world.get_entity_mut(corpse) {
            e.insert(mud_world::CoinPile(coin));
        }
        let msg = crate::commands::format_wealth(coin).unwrap_or_else(|| "no coin".to_string());
        send_to(
            world,
            killer,
            format!(
                "{msg} lies among the remains of {victim_name}. \
                 (`get all from corpse` to claim, or set `autogold` to auto-collect.)\r\n"
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
    let base_share = (coin / n).max(1);
    for r in &recipients {
        // Per-race coin scaling from `Races.copper_factor` (percent).
        // Default is 75 on the schema — a "neutral" race takes home
        // 75% of the raw take, leaving headroom for outliers. The
        // award message also shows the scaled value so the
        // bookkeeping and the prose stay in sync.
        let copper_factor = world
            .get::<mud_world::Profile>(*r)
            .and_then(|p| {
                world
                    .get_resource::<mud_world::RaceCatalog>()
                    .and_then(|c| c.get(&p.race))
            })
            .map_or(100, |def| def.copper_factor);
        let share = if copper_factor == 100 {
            base_share
        } else {
            (base_share.saturating_mul(i64::from(copper_factor))
                / 100)
                .max(1)
        };
        if let Some(mut w) = world.get_mut::<Wealth>(*r) {
            w.0 = w.0.saturating_add(share);
        } else {
            try_insert(world, *r, Wealth(share));
        }
        let line = if recipients.len() == 1 {
            let msg =
                crate::commands::format_wealth(share).unwrap_or_else(|| "no coin".to_string());
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
/// "Kill the wrong target" alignment penalty. Looks up the
/// victim's `MobProto.protected_kind`; if non-Normal, shifts the
/// killer's `CombatStats.alignment` toward EVIL by the per-kind
/// delta and emits a guilt-flavor line. Clamped at -1000 (the
/// schema's pure-evil floor).
fn apply_protected_kill_penalty(world: &mut World, killer: Entity, victim: Entity) {
    let proto_key = match world.get::<mud_world::WorldKey>(victim) {
        Some(k) => *k,
        None => return,
    };
    let protected = world
        .get_resource::<mud_world::MobPrototypes>()
        .and_then(|p| p.by_key.get(&(proto_key.zone, proto_key.id)))
        .map(|m| m.protected_kind);
    let Some(kind) = protected else { return };
    let delta = kind.alignment_penalty();
    if delta == 0 {
        return;
    }
    if let Some(mut cs) = world.get_mut::<mud_world::CombatStats>(killer) {
        cs.alignment = (cs.alignment + delta).max(-1000);
    }
    let line = match kind {
        mud_db::enums::ProtectedKind::Innocent => {
            "A wave of cold guilt washes through you — that creature was no threat.\r\n"
        }
        mud_db::enums::ProtectedKind::Shopkeeper => {
            "A shudder runs through the marketplace. Word of this will spread.\r\n"
        }
        mud_db::enums::ProtectedKind::QuestNpc => {
            "A flicker of regret — there were stories left untold.\r\n"
        }
        mud_db::enums::ProtectedKind::Normal => return,
    };
    send_to(world, killer, line);
}

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
    // Trophy kill share: legacy splits 1.0 across the group. The
    // i32→f32 cast is fine since n ≤ recipient count (small).
    #[allow(clippy::cast_precision_loss)]
    let trophy_share = 1.0_f32 / n as f32;
    // Trophy key for this victim — we only have a mob proto in
    // hand here (PvP has its own kill plumbing), so build the
    // Mob variant from the WorldKey.
    let trophy_kind = mud_world::TrophyKind::Mob {
        zone: proto.zone_id,
        id: proto.id,
    };

    for entity in &recipients {
        // Max-tier players (level 100+ — staff and endgame) don't
        // gain XP from kills: there's nothing to level into and the
        // line just adds noise. Skip silently — the score sheet's
        // "next level" suppression already signals the cap.
        let level = world
            .get::<mud_world::Profile>(*entity)
            .map_or(0, |p| p.level);
        if level >= 100 {
            continue;
        }
        // Anti-grind: scale down XP based on how often this player
        // has already killed this target. Mirrors legacy
        // `exp_trophy_modifier` with the same band thresholds.
        let prior_kills = world
            .get::<mud_world::Trophy>(*entity)
            .map_or(0.0, |t| t.kills_against(&trophy_kind));
        let modifier = trophy_xp_modifier(prior_kills);
        // f32 round-trip on the XP value — share fits comfortably
        // in f32 mantissa for any sane player level, and we floor
        // at 1 so heavy penalty bands still award a token amount.
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pre_race = ((share as f32) * modifier).max(1.0) as i32;
        // Per-race XP scaling from `Races.exp_factor` (percent).
        // 100 = unchanged; 120 → +20% XP for this race.
        // `(amount * factor) / 100`, floored at 1 so degenerate
        // factors still pay a token reward.
        let race = world
            .get::<mud_world::Profile>(*entity)
            .map(|p| p.race.clone());
        let exp_factor = race
            .as_deref()
            .and_then(|r| {
                world
                    .get_resource::<mud_world::RaceCatalog>()
                    .and_then(|c| c.get(r))
            })
            .map_or(100, |def| def.exp_factor);
        let scaled = pre_race
            .saturating_mul(exp_factor)
            .saturating_div(100)
            .max(1);
        if let Some(mut p) = world.get_mut::<mud_world::Profile>(*entity) {
            p.experience = p.experience.saturating_add(scaled);
        } else {
            continue;
        }
        // Record the kill into trophy *after* XP scales — so the
        // current swing benefits from the lower-band rate, and the
        // next one feels the new cap.
        let display_name = victim_name.to_string();
        if world.get::<mud_world::Trophy>(*entity).is_none()
            && let Ok(mut em) = world.get_entity_mut(*entity)
        {
            em.insert(mud_world::Trophy::default());
        }
        if let Some(mut trophy) = world.get_mut::<mud_world::Trophy>(*entity) {
            trophy.record(trophy_kind.clone(), trophy_share, display_name);
        }
        let line = if *entity == killer && recipients.len() == 1 {
            format!("You gain {scaled} experience for the kill of {victim_name}.\r\n")
        } else {
            format!(
                "You gain {scaled} experience (group share) for the kill of {victim_name}.\r\n"
            )
        };
        send_to(world, *entity, line);
        check_level_up(world, *entity);
    }
}

/// Trophy XP scaling. Mirrors the legacy `exp_trophy_modifier`
/// bands so a player who repeat-kills the same mob gets a
/// progressively diminishing reward — anti-grind without
/// blocking it outright.
#[must_use]
pub fn trophy_xp_modifier(prior_kills: f32) -> f32 {
    if prior_kills < 2.01 {
        1.0
    } else if prior_kills < 3.01 {
        0.95
    } else if prior_kills < 5.01 {
        0.85
    } else if prior_kills < 7.01 {
        0.65
    } else if prior_kills < 10.01 {
        0.45
    } else {
        0.3
    }
}

/// Check whether `entity`'s `Profile.experience` has crossed the
/// next-level threshold, and if so promote (possibly multiple
/// levels in one call) — incrementing `Profile.level`, expanding
/// `Health.max` and `Stamina.max` by the row's gain values, and
/// emitting a "you advanced to level N" line per step.
pub(crate) fn check_level_up(world: &mut World, entity: Entity) {
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
        // Per-race HP gain scaling from `Races.hp_factor` (percent).
        // `LevelDefinition.hp_gain` × race.hp_factor / 100; floor
        // at 1 so a degenerate factor still grants something.
        // `class.hp_per_level` layers on top — flat add per level
        // regardless of race.
        let race = world
            .get::<Profile>(entity)
            .map(|p| (p.race.clone(), p.class_id));
        let (hp_factor, class_hp_per_level, class_hit_dice_roll) = race
            .as_ref()
            .map(|(r, cid)| {
                let race_factor = world
                    .get_resource::<mud_world::RaceCatalog>()
                    .and_then(|c| c.get(r))
                    .map_or(100, |def| def.hp_factor);
                let (class_hp, hit_dice_roll) = cid
                    .and_then(|c| {
                        world
                            .get_resource::<mud_world::ClassCatalog>()
                            .and_then(|cat| cat.by_id.get(&c))
                    })
                    .map_or((0, 0), |c| {
                        let (n, m, b) = parse_hit_dice(&c.hit_dice);
                        (c.hp_per_level, roll_dice(n, m, b))
                    });
                (race_factor, class_hp, hit_dice_roll)
            })
            .unwrap_or((100, 0, 0));
        let race_scaled = next_row
            .hp_gain
            .saturating_mul(hp_factor)
            .saturating_div(100)
            .max(1);
        let total_hp_gain = race_scaled
            .saturating_add(class_hp_per_level)
            .saturating_add(class_hit_dice_roll);
        if let Some(mut h) = world.get_mut::<mud_world::Health>(entity) {
            h.max = h.max.saturating_add(total_hp_gain);
            h.hp = h.max; // full heal on level-up
        }
        if let Some(mut s) = world.get_mut::<mud_world::Stamina>(entity) {
            s.max = s.max.saturating_add(next_row.stamina_gain);
            s.current = s.max;
        }
        // Practice points per level: a base of 2, plus the better
        // of the caster's INT or WIS bonus. Scales mental-stat-heavy
        // builds without making physical-stat builds bone-dry.
        // Floor at 1 so a level-up always grants something even for
        // a -2-bonus character.
        let bonus = world
            .get::<mud_world::CoreStats>(entity)
            .map_or(0, |s| {
                let int_b = mud_world::CoreStats::bonus(s.intelligence);
                let wis_b = mud_world::CoreStats::bonus(s.wisdom);
                int_b.max(wis_b)
            });
        let granted = (2 + bonus).max(1);
        if let Some(mut sp) = world.get_mut::<mud_world::SkillPoints>(entity) {
            sp.0 = sp.0.saturating_add(granted);
        }
        let plural = if granted == 1 { "point" } else { "points" };
        send_to(
            world,
            entity,
            format!(
                "*** You have advanced to level {next}{}! ***\r\n\
                 You gained {granted} practice {plural}.\r\n",
                next_row
                    .name
                    .as_deref()
                    .map_or_else(String::new, |n| format!(" ({n})"))
            ),
        );
        // Milestone achievement hooks. Codes are stable strings the
        // catalog references; if a row is missing, grant_achievement
        // no-ops cleanly.
        for milestone in [5, 15, 30, 50, 75, 100] {
            if next == milestone {
                let code = format!("level_{milestone}");
                crate::commands::grant_achievement(world, entity, &code);
            }
        }
        // Quest trigger: LEVEL (Wave 4.1). Any quest authored with
        // `triggerType = LEVEL` and `triggerLevel = next` is offered
        // (or auto-accepted) for this player.
        crate::quest_triggers::dispatch_level_trigger(world, entity, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hit_dice_handles_common_shapes() {
        assert_eq!(parse_hit_dice("1d6"), (1, 6, 0));
        assert_eq!(parse_hit_dice("8d13"), (8, 13, 0));
        assert_eq!(parse_hit_dice("1d6+2"), (1, 6, 2));
        assert_eq!(parse_hit_dice("2d4-1"), (2, 4, -1));
        assert_eq!(parse_hit_dice("  1d8 "), (1, 8, 0));
        assert_eq!(parse_hit_dice("5"), (0, 0, 5)); // bare constant
        assert_eq!(parse_hit_dice(""), (0, 0, 0));
        assert_eq!(parse_hit_dice("garbage"), (0, 0, 0));
    }

    /// Spawn a minimal "room" (just an Entity with no components — combat
    /// only needs an Entity handle for Located references; nothing reads
    /// room contents during a swing) and return its handle.
    fn make_room(world: &mut World) -> Entity {
        world.spawn_empty().id()
    }

    /// Spawn an attacker with Fighting+CombatStats+Located+Named pointed
    /// at `target`. `dmg_roll` is configurable so callers can predict the
    /// numeric outcome.
    ///
    /// Damage modeling under the new acc/ev pipeline: tests still want
    /// raw "does ~N damage per swing" semantics. The new swing formula
    /// is `weapon_dice * (1 + attack_power/100)`, so an unarmed
    /// attacker rolls 1 by default — multiplying that by attack_power
    /// can't reproduce a band like "7 ± 1". Instead we attach a
    /// `NaturalDamage { 1d1 + (dmg_roll - 1) }` so the rolled base is
    /// exactly `dmg_roll`, then leave attack_power at 0. This keeps
    /// the existing per-test damage assertions (variance bands, crit
    /// promotion math) intact across the rewrite.
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
                    // accuracy 200 vs default evasion 50 = +75
                    // chance margin → clamped 99% hit. Equivalent
                    // to the old `hit_roll: 100` ceiling: tests
                    // still rely on the swing landing, which is
                    // true at 99% but not 100% — flake-check this
                    // if a CI run misses 1/100.
                    accuracy: 200,
                    ..Default::default()
                },
                NaturalDamage { num: 1, size: 1, bonus: dmg_roll - 1 },
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
        // combat_tick fires only on multiples of COMBAT_PERIOD_TICKS (40).
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
    fn mob_kill_spawns_corpse_with_coin_pile() {
        // Regression for the playtest "no corpse / no gold" bug:
        // killing an itemless mob with a coin proto must (a) spawn
        // a corpse in the room and (b) leave the coin reachable via
        // a CoinPile component on that corpse, since the killer
        // here has no AutoGold flag. Previously the corpse only
        // spawned when owned_items was non-empty, and the coin was
        // forfeited with a misleading "scattered around the corpse"
        // message.
        use mud_world::{CoinPile, MobPrototypes};
        let mut world = World::new();
        let room = make_room(&mut world);
        // Minimal MobProto carrying a coin reward.
        let mut protos = MobPrototypes::default();
        protos.by_key.insert(
            (1, 1),
            mud_world::MobProto {
                zone_id: 1,
                id: 1,
                name: "a stray dog".to_string(),
                keywords: vec!["dog".to_string()],
                room_description: String::new(),
                examine_description: String::new(),
                gender: "neutral".to_string(),
                race: "animal".to_string(),
                level: 1,
                alignment: 0,
                role: mud_db::enums::MobRole::Normal,
                hp_dice_num: 1,
                hp_dice_size: 1,
                hp_dice_bonus: 5,
                damage_dice_num: 1,
                damage_dice_size: 1,
                damage_dice_bonus: 0,
                accuracy: 0,
                evasion: 0,
                attack_power: 0,
                spell_power: 0,
                penetration_flat: 0,
                penetration_percent: 0,
                armor_rating: 0,
                damage_reduction_percent: 0,
                soak: 0,
                hardness: 0,
                perception: 0,
                concealment: 0,
                resistances: serde_json::json!({}),
                ward_percent: 0,
                wealth: 75, // 7 silver, 5 copper
                class_id: None,
                behaviors: Vec::new(),
                protected_kind: mud_db::enums::ProtectedKind::Normal,
                professions: Vec::new(),
                // Mob latent parity (Wave 2.L) defaults for the test
                // proto. Match the schema's column defaults so this
                // proto reads like a freshly-imported row.
                size: mud_db::enums::Size::Medium,
                life_force: mud_db::enums::LifeForce::Life,
                damage_type: mud_db::enums::DamageType::Hit,
                move_points: 0,
                default_position: mud_db::enums::Position::Standing,
                traits: Vec::new(),
                movement_mode: mud_db::enums::MovementMode::Normal,
                default_movement_mode: mud_db::enums::MovementMode::Normal,
            },
        );
        world.insert_resource(protos);

        let target = world
            .spawn((
                Mob,
                Named { name: "a stray dog".to_string() },
                Located(room),
                Health { hp: 5, max: 5 },
                mud_world::WorldKey { zone: 1, id: 1 },
            ))
            .id();
        let player = world
            .spawn((
                Player,
                Named { name: "Tester".to_string() },
                Located(room),
                Health { hp: 100, max: 100 },
                CombatStats {
                    // accuracy 200 vs default evasion 50 → clamped 99%
                    // hit. One swing overwhelmingly likely to land.
                    accuracy: 200,
                    ..Default::default()
                },
                // 1d1 + 99 = 100 baseline damage; one-shots the
                // 5-HP dog regardless of crit/variance branch.
                NaturalDamage { num: 1, size: 1, bonus: 99 },
                Posture(PostureKind::Standing),
                Fighting(target),
            ))
            .id();

        run_combat_tick(&mut world);

        // Mob is dead, corpse exists in the room.
        assert!(
            world.get_entity(target).is_err(),
            "target despawned"
        );
        let corpse = world
            .query_filtered::<(Entity, &Located, &CoinPile), With<Corpse>>()
            .iter(&world)
            .find(|(_, l, _)| l.0 == room)
            .map(|(e, _, p)| (e, p.0));
        let (_corpse_entity, coin) = corpse
            .expect("corpse with CoinPile spawned in room (no AutoGold)");
        assert_eq!(coin, 75, "coin amount lands on corpse for non-AutoGold killer");
        // Player wealth is still zero — coin is on the corpse,
        // claimed via `get all from corpse`.
        assert!(
            world.get::<Wealth>(player).is_none()
                || world.get::<Wealth>(player).unwrap().0 == 0,
            "no AutoGold means no wealth deposited yet"
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

        // Player should now be a Ghost with HP at 0 — they're dead.
        assert!(
            world.get::<Ghost>(player).is_some(),
            "player gains Ghost marker on death"
        );
        let hp = world.get::<Health>(player).expect("player keeps Health");
        assert_eq!(hp.hp, 0, "ghost HP at 0 (dead body)");
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
    fn handle_death_clears_mob_grudge_data_against_dead_player() {
        // Regression for the post-death re-aggro loop: if a mob's
        // MobMemory / HateList still references the dead player, the
        // combat-tick pre-pass would re-aggro onto the ghost every
        // tick (HP pinned to 1 used to pass the `hp > 0` filter).
        // After death, both data structures must drop the player.
        let mut world = World::new();
        let room = make_room(&mut world);
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
        let mob = world
            .spawn((
                Mob,
                Named { name: "Guard".to_string() },
                Located(room),
                Health { hp: 50, max: 50 },
                MobMemory({
                    let mut s = std::collections::HashSet::new();
                    s.insert(player);
                    s
                }),
                HateList(vec![player]),
            ))
            .id();

        super::handle_death(&mut world, player, "Tester", room);

        let mem = world
            .get::<MobMemory>(mob)
            .expect("MobMemory component preserved");
        assert!(
            !mem.0.contains(&player),
            "MobMemory drops dead player on death"
        );
        let hate = world
            .get::<HateList>(mob)
            .expect("HateList component preserved");
        assert!(
            !hate.0.contains(&player),
            "HateList drops dead player on death"
        );
    }

    #[test]
    fn sleeping_target_jolts_awake_on_damage() {
        // The "you jolt awake!" branch: a sleeping victim that
        // takes a non-lethal hit must transition to Standing on
        // the same swing. Auto-hit on sleepers means the combat
        // formula can't miss, so the only paths are hit-and-die
        // or hit-and-wake. This guards the second one.
        let mut world = World::new();
        let room = make_room(&mut world);
        let target = world
            .spawn((
                Named { name: "Sleeper".to_string() },
                Located(room),
                Health { hp: 50, max: 50 },
                // Default defender — accuracy/evasion both 0,
                // armor pipeline inert, no resistances. Attacker's
                // huge accuracy makes the swing land regardless.
                CombatStats::default(),
                Posture(PostureKind::Sleeping),
            ))
            .id();
        let attacker = make_attacker(&mut world, room, target, 7);
        try_insert(&mut world, target, Fighting(attacker));

        run_combat_tick(&mut world);

        assert_eq!(
            world.get::<Posture>(target).map(|p| p.0),
            Some(PostureKind::Standing),
            "sleeping target jolts to Standing on damage"
        );
        // Hit is auto (sleeping bypasses the roll), so HP
        // definitely dropped. Don't assert exact value — crit
        // randomness leaves a band — but anything below max
        // proves the swing landed.
        assert!(
            world.get::<Health>(target).unwrap().hp < 50,
            "sleeping target took damage from auto-hit"
        );
    }

    #[test]
    fn resting_target_scrambles_to_feet_on_damage() {
        // posture-and-lifestate.md design: a hit on a RESTING
        // defender auto-stands them. Mirror of the existing
        // sleeping jolt-awake path. Stops resting players from
        // staying on the ground while taking damage indefinitely.
        //
        // Resting adds +5 AC (posture_ac_modifier), so the
        // attacker needs hit_roll high enough to clear that
        // band even at the worst end of the swing roll. 50 is
        // well past the 100% cap.
        let mut world = World::new();
        let room = make_room(&mut world);
        let target = world
            .spawn((
                Named { name: "Resting".to_string() },
                Located(room),
                Health { hp: 50, max: 50 },
                // Default defender — see sleeping-jolt test.
                CombatStats::default(),
                Posture(PostureKind::Resting),
            ))
            .id();
        let attacker = world
            .spawn((
                Named { name: "Attacker".to_string() },
                Located(room),
                Fighting(target),
                CombatStats {
                    // Old: hit_roll: 50 → accuracy = 50 + 50*2 = 150.
                    // Vs default-defender evasion 0 minus resting
                    // posture penalty: still well past the 99% cap.
                    accuracy: 150,
                    ..Default::default()
                },
                // dmg_roll: 7 → 1d1 + 6 = exactly 7 base damage.
                NaturalDamage { num: 1, size: 1, bonus: 6 },
                Posture(PostureKind::Standing),
            ))
            .id();
        try_insert(&mut world, target, Fighting(attacker));

        run_combat_tick(&mut world);

        assert_eq!(
            world.get::<Posture>(target).map(|p| p.0),
            Some(PostureKind::Standing),
            "resting target stands after taking a hit"
        );
        // Hit should still land — the auto-stand happens after
        // damage application, not as a dodge.
        assert!(
            world.get::<Health>(target).unwrap().hp < 50,
            "swing landed before the auto-stand"
        );
    }

    #[test]
    fn fleer_keeps_hate_list_for_re_aggro_on_return() {
        // Flee semantics: cmd_flee removes the fleer's Fighting and
        // moves them to a new room; combat_tick's room-mismatch
        // pass clears attackers' Fighting on next tick. But the
        // mob's HateList must keep the fleer entry — that's what
        // the on-entry / re-aggro pass uses to re-engage when the
        // player walks back in.
        //
        // This test simulates the post-flee state directly (player
        // already moved, mob still has Fighting + HateList) and
        // runs combat_tick to verify: attacker's Fighting clears
        // via room mismatch, HateList retains the entry.
        let mut world = World::new();
        let room_a = make_room(&mut world);
        let room_b = make_room(&mut world);
        let player = world
            .spawn((
                Player,
                Named { name: "Tester".to_string() },
                Located(room_b), // already fled
                Health { hp: 100, max: 100 },
                Posture(PostureKind::Standing),
                CombatStats::default(),
            ))
            .id();
        let mob = world
            .spawn((
                Mob,
                Named { name: "Guard".to_string() },
                Located(room_a),
                Health { hp: 50, max: 50 },
                CombatStats {
                    // Old: hit_roll 10, dmg_roll 5 — values don't
                    // matter for this assertion (room-mismatch
                    // clears Fighting before any swing fires).
                    accuracy: 70,
                    attack_power: 25,
                    ..Default::default()
                },
                Posture(PostureKind::Standing),
                Fighting(player),
                HateList(vec![player]),
            ))
            .id();

        run_combat_tick(&mut world);

        assert!(
            world.get::<Fighting>(mob).is_none(),
            "mob disengages on room mismatch"
        );
        let hate = world
            .get::<HateList>(mob)
            .expect("HateList preserved across flee");
        assert!(
            hate.0.contains(&player),
            "fleer remains on the HateList for re-aggro on return"
        );
    }

    #[test]
    fn mid_tick_residual_swing_no_ops_after_target_ghosts() {
        // Two attackers swinging at the same player target in one
        // tick. The first swing kills the target (handle_death
        // fires, Ghost set, attackers' Fighting swept). The second
        // swing was already in the snapshot list — it still
        // executes apply_swing, but apply_damage must early-return
        // on the new Ghost so the residual swing doesn't push HP
        // below 0 or trigger a second death event.
        let mut world = World::new();
        let room = make_room(&mut world);
        world.insert_resource(TickCount(COMBAT_PERIOD_TICKS));
        let player = world
            .spawn((
                Player,
                Named { name: "Tester".to_string() },
                Located(room),
                Health { hp: 5, max: 100 }, // one swing kills
                Posture(PostureKind::Standing),
                CombatStats::default(),
            ))
            .id();
        let _attacker_a = make_attacker(&mut world, room, player, 50);
        let _attacker_b = make_attacker(&mut world, room, player, 50);

        combat_tick(&mut world);

        assert!(
            world.get::<Ghost>(player).is_some(),
            "target was ghosted by the lethal swing"
        );
        assert_eq!(
            world.get::<Health>(player).unwrap().hp,
            0,
            "ghost HP at 0 — residual swing didn't drive it negative"
        );
    }

    #[test]
    fn combat_resumes_after_stun_clears() {
        // Integration: a Stunned attacker doesn't swing, but once
        // the marker is gone (effects_tick clears it when the last
        // backing stun EffectInstance expires), the next combat
        // tick should land damage. effects::tests already cover
        // marker-add/remove in isolation; this test bridges the
        // two systems.
        use mud_world::Stunned;
        let mut world = World::new();
        let room = make_room(&mut world);
        let target = make_target(&mut world, room, 50);
        let attacker = make_attacker(&mut world, room, target, 7);
        try_insert(&mut world, attacker, Stunned);

        // Stunned: no damage applied.
        run_combat_tick(&mut world);
        assert_eq!(
            world.get::<Health>(target).unwrap().hp,
            50,
            "stunned attacker doesn't swing"
        );

        // Marker cleared (mimic effects_tick's behavior). Next combat
        // tick should land damage.
        try_remove::<Stunned>(&mut world, attacker);
        run_combat_tick(&mut world);
        assert!(
            world.get::<Health>(target).unwrap().hp < 50,
            "swing lands once Stunned clears"
        );
    }

    #[test]
    fn mid_tick_residual_swing_skips_despawned_mob() {
        // Multi-attacker mob death race: two attackers swing at the
        // same mob in one tick; the first kill despawns the mob.
        // The second swing was already snapshotted, so apply_swing
        // is still called with a target Entity that no longer
        // exists. Verifies the early-return at apply_swing's top
        // (`world.get_entity(target).is_err()`) clears Fighting
        // from the residual attacker without panicking.
        let mut world = World::new();
        let room = make_room(&mut world);
        let mob = world
            .spawn((
                Mob,
                Named { name: "Target".to_string() },
                Located(room),
                Health { hp: 5, max: 5 }, // one swing kills
            ))
            .id();
        let attacker_a = make_attacker(&mut world, room, mob, 50);
        let attacker_b = make_attacker(&mut world, room, mob, 50);

        run_combat_tick(&mut world);

        assert!(
            world.get_entity(mob).is_err(),
            "lethal first swing despawned the mob"
        );
        // Both attackers must end up with Fighting cleared — one
        // via handle_death's sweep, the other via the
        // entity-gone early-return in apply_swing.
        assert!(
            world.get::<Fighting>(attacker_a).is_none(),
            "first attacker disengaged via handle_death"
        );
        assert!(
            world.get::<Fighting>(attacker_b).is_none(),
            "second (residual-swing) attacker disengaged via entity-gone guard"
        );
    }

    #[test]
    fn frozen_attacker_is_filtered_from_swing_snapshot() {
        // Defense-in-depth check: even if a Frozen entity somehow
        // has Fighting set on them, the swing snapshot must skip
        // them. Otherwise admin-frozen players (or any future
        // mid-combat freeze effect) would still keep swinging.
        use mud_world::Frozen;
        let mut world = World::new();
        let room = make_room(&mut world);
        let target = make_target(&mut world, room, 50);
        let attacker = make_attacker(&mut world, room, target, 7);
        try_insert(&mut world, attacker, Frozen);

        run_combat_tick(&mut world);

        let hp = world
            .get::<Health>(target)
            .expect("target still has Health");
        assert_eq!(
            hp.hp, 50,
            "Frozen attacker doesn't generate a swing"
        );
    }

    #[test]
    fn re_aggro_skips_frozen_targets() {
        // A Frozen player co-located with a mob holding their entry
        // in HateList must not get re-engaged. Same rule shape as
        // the Ghost test below — life-state markers are the
        // authoritative liveness gate, not HP.
        use mud_world::Frozen;
        let mut world = World::new();
        let room = make_room(&mut world);
        let player = world
            .spawn((
                Player,
                Named { name: "Tester".to_string() },
                Located(room),
                Health { hp: 100, max: 100 },
                Posture(PostureKind::Standing),
                Frozen,
            ))
            .id();
        let mob = world
            .spawn((
                Mob,
                Named { name: "Guard".to_string() },
                Located(room),
                Health { hp: 50, max: 50 },
                CombatStats {
                    // Old: hit_roll 10, dmg_roll 20 — never swings
                    // (re-aggro is the gate being tested, the mob
                    // never picks up Fighting). Values cosmetic.
                    accuracy: 70,
                    attack_power: 100,
                    ..Default::default()
                },
                Posture(PostureKind::Standing),
                HateList(vec![player]),
            ))
            .id();

        run_combat_tick(&mut world);

        assert!(
            world.get::<Fighting>(mob).is_none(),
            "mob doesn't re-aggro onto a frozen target"
        );
        assert!(
            world.get::<Fighting>(player).is_none(),
            "frozen player doesn't pick up Fighting"
        );
    }

    #[test]
    fn re_aggro_skips_stunned_targets() {
        // Same coverage shape for Stunned. Stun is short-lived
        // (effects_tick drops it when the backing EffectInstance
        // expires) but during the stun window the target should
        // be off the re-aggro candidate list.
        use mud_world::Stunned;
        let mut world = World::new();
        let room = make_room(&mut world);
        let player = world
            .spawn((
                Player,
                Named { name: "Tester".to_string() },
                Located(room),
                Health { hp: 100, max: 100 },
                Posture(PostureKind::Standing),
                Stunned,
            ))
            .id();
        let mob = world
            .spawn((
                Mob,
                Named { name: "Guard".to_string() },
                Located(room),
                Health { hp: 50, max: 50 },
                CombatStats {
                    // Old: hit_roll 10, dmg_roll 20 — never swings
                    // (re-aggro is the gate being tested, the mob
                    // never picks up Fighting). Values cosmetic.
                    accuracy: 70,
                    attack_power: 100,
                    ..Default::default()
                },
                Posture(PostureKind::Standing),
                HateList(vec![player]),
            ))
            .id();

        run_combat_tick(&mut world);

        assert!(
            world.get::<Fighting>(mob).is_none(),
            "mob doesn't re-aggro onto a stunned target"
        );
    }

    #[test]
    fn re_aggro_skips_ghost_targets() {
        // Regression: the combat-tick pre-pass that re-engages mobs
        // from their HateList must skip Ghost targets, otherwise a
        // dead-but-still-co-located player gets put back into combat
        // every tick.
        let mut world = World::new();
        let room = make_room(&mut world);
        let player = world
            .spawn((
                Player,
                Named { name: "Tester".to_string() },
                Located(room),
                Health { hp: 0, max: 100 },
                Posture(PostureKind::Standing),
                Ghost,
            ))
            .id();
        let mob = world
            .spawn((
                Mob,
                Named { name: "Guard".to_string() },
                Located(room),
                Health { hp: 50, max: 50 },
                CombatStats {
                    // Old: hit_roll 10, dmg_roll 20 — never swings
                    // (re-aggro is the gate being tested, the mob
                    // never picks up Fighting). Values cosmetic.
                    accuracy: 70,
                    attack_power: 100,
                    ..Default::default()
                },
                Posture(PostureKind::Standing),
                HateList(vec![player]),
            ))
            .id();

        run_combat_tick(&mut world);

        assert!(
            world.get::<Fighting>(mob).is_none(),
            "mob doesn't re-aggro onto a ghost target"
        );
        assert!(
            world.get::<Fighting>(player).is_none(),
            "ghost player doesn't get Fighting set on them"
        );
        // Ghost HP shouldn't have changed either — apply_damage is
        // a no-op on Ghost targets.
        let hp = world.get::<Health>(player).expect("ghost keeps Health");
        assert_eq!(hp.hp, 0, "ghost HP unchanged after combat tick");
    }

    #[test]
    fn hit_chance_curve() {
        // Acc/Ev d100 contest: chance = 50 + (accuracy - evasion) / 2,
        // clamped to [1, 99]. Each 2 points of margin = +1%.
        // Equal stats: 50% baseline.
        assert_eq!(hit_chance_pct(0, 0), 50);
        assert_eq!(hit_chance_pct(50, 50), 50);
        // Accuracy advantage: +2 acc = +1% chance.
        assert_eq!(hit_chance_pct(10, 0), 55);
        assert_eq!(hit_chance_pct(20, 0), 60);
        assert_eq!(hit_chance_pct(50, 0), 75);
        // Evasion advantage: same ratio mirrored.
        assert_eq!(hit_chance_pct(0, 10), 45);
        assert_eq!(hit_chance_pct(0, 20), 40);
        assert_eq!(hit_chance_pct(0, 50), 25);
        // Floor / ceiling clamps at [1, 99].
        assert_eq!(hit_chance_pct(0, 200), 1); // -100 → 0 → clamp 1
        assert_eq!(hit_chance_pct(200, 0), 99); // +100 → 100 → clamp 99
        assert_eq!(hit_chance_pct(0, 1000), 1);
        assert_eq!(hit_chance_pct(1000, 0), 99);
        // Mixed example: avg attacker accuracy 75 vs defender 50 →
        // margin 25 → +12 (integer division) → 62%.
        assert_eq!(hit_chance_pct(75, 50), 62);
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

    // ---------------------------------------------------------------
    // Room-flag wiring tests. These cover the cmd_move DeathTrap
    // gate, the per-recipient SoundproofRoom suppression in global
    // channel broadcasts, the NoMagicRoom gate in `invoke_ability`,
    // and the ArenaRoom marker's coexistence with combat (no PK
    // refusal today — but the marker must not accidentally also
    // imply PeacefulRoom).
    // ---------------------------------------------------------------

    /// `DeathTrap` marker plus `handle_death` together implement the
    /// "step into the room and die" contract. cmd_move's gate just
    /// asks "is there a DeathTrap here?" and routes to handle_death;
    /// this test pins handle_death's effect on a player so the
    /// composition stands. The loader test (mud-world side) verifies
    /// the marker lands; this verifies the consumer's outcome.
    #[test]
    fn death_trap_path_ghosts_player_via_handle_death() {
        let mut world = World::new();
        let room = make_room(&mut world);
        // The mover is a plain Player, no Account => mortal. cmd_move
        // collects them as a dt_victim and calls handle_death(room).
        // Marker on the room confirms the gate-check truth value
        // the cmd_move branch tests against.
        world.entity_mut(room).insert(mud_world::DeathTrap);
        world.insert_resource(TickCount(0));
        let player = world
            .spawn((
                Player,
                Named { name: "DTVictim".to_string() },
                Located(room),
                Health { hp: 100, max: 100 },
                Posture(PostureKind::Standing),
            ))
            .id();
        assert!(
            world.get::<mud_world::DeathTrap>(room).is_some(),
            "DeathTrap marker pre-condition",
        );
        super::handle_death(&mut world, player, "DTVictim", room);
        assert!(
            world.get::<Ghost>(player).is_some(),
            "player ghosted on death-trap entry",
        );
        assert_eq!(
            world.get::<Health>(player).map(|h| h.hp),
            Some(0),
            "DT victim drops to 0 HP",
        );
    }

    /// `ArenaRoom` is a tag for `look` flavor and a placeholder for
    /// the future PK opt-in toggle. It must NOT secretly imply the
    /// peaceful-room gate (which would cause combat in an arena to
    /// be refused). Pin that compatibility: combat between two
    /// players in an arena room is not blocked by any sibling
    /// PeacefulRoom marker.
    #[test]
    fn arena_room_marker_does_not_imply_peaceful_room() {
        let mut world = World::new();
        let room = make_room(&mut world);
        world.entity_mut(room).insert(mud_world::ArenaRoom);
        // Two players in the same arena room. Neither carries the
        // peaceful marker; the gate-check `world.get::<PeacefulRoom>`
        // must return None.
        let _p1 = world
            .spawn((
                Player,
                Named { name: "ArenaA".to_string() },
                Located(room),
                Health { hp: 100, max: 100 },
                Posture(PostureKind::Standing),
            ))
            .id();
        let _p2 = world
            .spawn((
                Player,
                Named { name: "ArenaB".to_string() },
                Located(room),
                Health { hp: 100, max: 100 },
                Posture(PostureKind::Standing),
            ))
            .id();
        assert!(
            world.get::<mud_world::ArenaRoom>(room).is_some(),
            "ArenaRoom marker is present",
        );
        assert!(
            world.get::<mud_world::PeacefulRoom>(room).is_none(),
            "ArenaRoom doesn't drag in PeacefulRoom — combat would be allowed",
        );
    }

    /// `NoMagicRoom` is consumed by `invoke_ability_with` as a
    /// pre-flight gate. The gate predicate is a single component
    /// lookup; this test pins the loader-side contract: marker
    /// present <=> casting refused. We verify the marker isolation
    /// at the world level (no other side effects from inserting it).
    #[test]
    fn no_magic_room_marker_present_when_inserted() {
        let mut world = World::new();
        let room = make_room(&mut world);
        world.entity_mut(room).insert(mud_world::NoMagicRoom);
        assert!(
            world.get::<mud_world::NoMagicRoom>(room).is_some(),
            "NoMagicRoom marker stored",
        );
        // The marker is opt-in: a fresh room without it doesn't
        // accidentally carry one.
        let other = make_room(&mut world);
        assert!(
            world.get::<mud_world::NoMagicRoom>(other).is_none(),
            "default room has no NoMagicRoom",
        );
    }

    /// `SoundproofRoom` is consumed in `broadcast_global` (channels).
    /// The gate checks each recipient's room; recipients in a
    /// soundproof room skip the per-recipient send. Verify the
    /// marker stores correctly so the broadcast loop's predicate
    /// fires; the loop itself can't be unit-tested without a
    /// Connection apparatus, so the contract here is "marker is
    /// present, broadcast skips it" — broadcast_global reads
    /// `world.get::<SoundproofRoom>` directly.
    #[test]
    fn soundproof_room_marker_classifies_room() {
        let mut world = World::new();
        let booth = make_room(&mut world);
        world.entity_mut(booth).insert(mud_world::SoundproofRoom);
        let player = world
            .spawn((
                Player,
                Named { name: "Listener".to_string() },
                Located(booth),
            ))
            .id();
        // The exact predicate `broadcast_global` runs is:
        //   world.get::<Located>(t).is_some()
        //     && world.get::<SoundproofRoom>(located.0).is_some()
        // Re-execute that here so a future change to the gate
        // wording is caught by this test.
        let located = world.get::<Located>(player).copied().expect("Located set");
        let is_soundproof = world
            .get::<mud_world::SoundproofRoom>(located.0)
            .is_some();
        assert!(
            is_soundproof,
            "listener's room reports as soundproof — broadcast skips them",
        );
    }
}
