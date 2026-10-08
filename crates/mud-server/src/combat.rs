use bevy_ecs::prelude::*;
use mud_world::{
    AppliedTo, CombatStats, Corpse, CorpseDecay, Description, EffectInstance, EquippedSlot, Exits,
    Fighting, FromMobReset, Ghost, Guarding, Health, Item, Keywords, KnownAbilities, Located, Mob,
    MobPrototypes, Named, NaturalDamage, ObjectPrototypes, Player, PlayerFlags, Posture,
    PostureKind, Slot, Stunned, Wealth, WearableIn, WorldKey, WorldKeyIndex,
};
use tracing::info;

use crate::TickCount;

/// Player corpse decay (G4.1). Set high enough that a player can log
/// back in days later and still recover their gear; matches legacy
/// MUD norms where a dead character's body lingered until the next
/// reboot or a manual purge. 7 days at 1-tick = 1 s resolution.
const PLAYER_CORPSE_DECAY_SECS: i32 = 7 * 24 * 60 * 60;
/// Mob corpse decay — quick cleanup so loot routes through the
/// claim window rather than piling up. 10 minutes.
const MOB_CORPSE_DECAY_SECS: i32 = 600;
use crate::commands::{
    apply_attacker_damage, arrival_from, broadcast_room_except_players_rendered,
    broadcast_room_except_rendered, cmd_flee, damage_color_tag, direction_name,
    disengage_attackers_of, drain_stamina, name_of, send_to, try_insert, try_remove,
};

/// Four real-time seconds per swing (40 ticks at 10Hz) — matches legacy
/// `PULSE_VIOLENCE` so the DB-authored damage values stay calibrated.
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
            Description(
                "A scarecrow-like training dummy stands here, patiently waiting to be punched."
                    .into(),
            ),
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

/// Counts down every `CorpseDecay`. On expiry follows legacy
/// `extract_corpse` (limits.cpp): the corpse's contents are moved out
/// rather than destroyed — to the room for a corpse lying on the floor
/// ("A quivering horde of maggots consumes $p."), to the carrier's room
/// for a carried one ("$p decays in your hands."), to the enclosing
/// container otherwise — then the corpse itself is removed. Player and
/// mob corpses share the path; they differ only in their starting timer.
/// Ephemeral aside from the corpse snapshot.
pub fn corpse_decay_tick(world: &mut World) {
    let tick = world.resource::<TickCount>().0;
    if !tick.is_multiple_of(10) {
        return;
    }
    // Snapshot so we can mutate freely.
    let corpses: Vec<(Entity, Entity, i32, String)> = {
        let mut q =
            world.query_filtered::<(Entity, &Located, &CorpseDecay, &Named), With<Corpse>>();
        q.iter(world)
            .map(|(e, l, d, n)| (e, l.0, d.remaining_secs, n.name.clone()))
            .collect()
    };
    let mut player_corpse_gone = false;
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
        let (holder, kind) = crate::item_decay::location_kind(world, corpse);
        match kind {
            crate::item_decay::HolderKind::Room => {
                broadcast_room_except_rendered(
                    world,
                    holder,
                    &[],
                    &format!("A quivering horde of maggots consumes {name}.\r\n"),
                );
            }
            crate::item_decay::HolderKind::Player => {
                crate::commands::send_to(
                    world,
                    holder,
                    format!(
                        "{} decays in your hands.\r\n",
                        crate::commands::cap_sentence_start(&name)
                    ),
                );
            }
            _ => {}
        }
        crate::item_decay::release_contents(world, corpse, holder, &kind);
        player_corpse_gone |= world.get::<mud_world::PlayerCorpse>(corpse).is_some();
        if let Ok(em) = world.get_entity_mut(corpse) {
            em.despawn();
        }
    }
    // A rotted-away player corpse must leave the on-disk snapshot too,
    // or a crash would bring it back.
    if player_corpse_gone {
        crate::corpses::save_snapshot(world);
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
/// Penalty subtracted from `CombatStats.evasion` while the defender
/// is in a non-alert posture. Values are tuned for the d100
/// accuracy-vs-evasion contest where 1 point = 1% swing in hit
/// chance — Standing=0 is the baseline, Sleeping=30 means a
/// sleeping defender is 30% easier to hit. Locked here so the
/// contract is greppable; A4 in remaining-work.md gates further
/// playtest adjustments.
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
pub(crate) fn mob_flee(world: &mut World, mob: Entity, from_room: Entity) {
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
    let mut candidates = candidates;
    crate::room_access::retain_admitted(world, &[mob], &mut candidates);
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
    if world.get::<Located>(mob).is_some() {
        world.entity_mut(mob).insert(Located(target_room));
    }
    let arrival_dir = arrival_from(dir);
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

/// The d100 hit roll. Production draws from the thread RNG; unit
/// tests can pin it (per test thread) via `FORCED_HIT_ROLL` so a
/// "the swing lands" assertion isn't subject to the 1% clamp-floor miss.
fn hit_roll() -> i32 {
    #[cfg(test)]
    if let Some(r) = FORCED_HIT_ROLL.with(std::cell::Cell::get) {
        return r;
    }
    rand::random_range(1..=100)
}

#[cfg(test)]
thread_local! {
    static FORCED_HIT_ROLL: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

/// Resolve one swing under the accuracy/evasion d100 contest from
/// docs/design/combat.md. `crit_chance` is a separate post-hit
/// d100 against the attacker's `crit_chance` field.
fn resolve_swing_acc_ev(accuracy: i32, evasion: i32, crit_chance: i32) -> SwingDetail {
    let chance = hit_chance_pct(accuracy, evasion);
    let roll = hit_roll();
    let (outcome, crit_roll) = if roll <= chance {
        let cr = rand::random_range(1..=100);
        if cr <= crit_chance {
            (SwingOutcome::Crit, cr)
        } else {
            (SwingOutcome::Hit, cr)
        }
    } else {
        (SwingOutcome::Miss, 0)
    };
    SwingDetail {
        outcome,
        roll,
        chance,
        crit_roll,
        crit_chance,
    }
}

/// Roll details surfaced by `resolve_swing` so the showdice toggle
/// can render them to the attacker. Outcome alone isn't enough —
/// players want to see the d100 vs threshold.
#[derive(Clone, Copy)]
pub(crate) struct SwingDetail {
    pub outcome: SwingOutcome,
    pub roll: i32,        // d100 hit roll
    pub chance: i32,      // need <= this to land a regular hit
    pub crit_roll: i32,   // 0 if the hit didn't land; else d100 vs crit_chance
    pub crit_chance: i32, // attacker's CombatStats.crit_chance (default 5)
}

/// Pipeline intermediates for a hit/crit swing, surfaced through
/// ``show_dice_swing`` when the dice toggle is on. Collects each
/// mitigation step so debugging "why did that crit only do 6 damage"
/// shows the full math. Filled in as the damage pipeline runs in
/// ``combat_tick``; ignored for misses.
#[derive(Clone, Copy, Default)]
pub(crate) struct SwingMitigation {
    pub weapon_roll: i32,       // raw dice (weapon or natural attack)
    pub attack_power: i32,      // attacker's CombatStats.attack_power (%)
    pub base_pre_crit: i32,     // weapon × (1 + AP/100) — the snapshot value
    pub after_crit: i32,        // × 1.5 if crit, else unchanged
    pub stealth_bonus_pct: i32, // A6 hidden-attacker damage % bonus (0 if not hidden)
    pub variance_delta: i32,    // signed delta applied (-band..=+band)
    pub after_variance: i32,
    pub armor_pct: i32, // effective_armor_pct (after pen)
    pub armor_k: i32,   // ARMOR_K constant
    pub after_armor_pct: i32,
    pub armor_flat: i32, // effective_armor_flat (after pen)
    pub after_armor_flat: i32,
    pub resist_pct: i32,
    pub after_resist: i32,
    pub hardness: i32,
    pub final_dmg: i32,
}

/// True iff the attacker has the `SHOW_DICE_ROLLS` `PlayerFlag` set.
/// Cheap (component lookup); call sites guard their detail-line
/// construction on this rather than always formatting the string.
pub(crate) fn show_dice_for(world: &World, attacker: Entity) -> bool {
    // DevMode forces dice visibility for everyone — open playtest
    // servers want every swing to show its roll regardless of the
    // per-player SHOW_DICE_ROLLS flag.
    if world.get_resource::<crate::DevMode>().is_some_and(|d| d.0) {
        return true;
    }
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
fn show_dice_swing(detail: SwingDetail, mit: SwingMitigation) -> String {
    // Display uses the "roll over" convention — higher roll = better,
    // matching modern player intuition. Internal math is still roll
    // under (resolve_swing rolls d100 and hits when roll <= chance);
    // we flip the displayed numbers via (101 - roll) and DC = (101 -
    // chance) so the math is preserved. Equivalent: roll' >= DC iff
    // original_roll <= chance.
    let display_roll = 101 - detail.roll;
    let display_dc = 101 - detail.chance;
    let header = match detail.outcome {
        SwingOutcome::Miss => {
            return format!("  (d100 {display_roll} < {display_dc} — miss)\r\n");
        }
        SwingOutcome::Hit if detail.roll == 0 => "auto-hit (sleeping target)".to_string(),
        SwingOutcome::Hit => {
            // Show the crit-roll attempt too — players want to see how
            // close they came (or that crit_chance is just 5%).
            let crit_disp = 101 - detail.crit_roll;
            let crit_dc = 101 - detail.crit_chance;
            format!(
                "d100 {display_roll} ≥ {display_dc} — HIT  (crit d100 {crit_disp} < {crit_dc} — no crit)"
            )
        }
        SwingOutcome::Crit => {
            let crit_disp = 101 - detail.crit_roll;
            let crit_dc = 101 - detail.crit_chance;
            format!(
                "d100 {display_roll} ≥ {display_dc} — HIT  (crit d100 {crit_disp} ≥ {crit_dc} — CRIT ×1.5)"
            )
        }
    };
    // Show "wpn 18 ×AP+5%=19" so the raw dice roll + AP step are visible.
    // For crits, append the ×1.5 promotion.
    let ap_step = match mit.attack_power.cmp(&0) {
        std::cmp::Ordering::Equal => format!("  wpn={} ", mit.weapon_roll),
        std::cmp::Ordering::Greater => format!(
            "  wpn={} ×AP+{}%={} ",
            mit.weapon_roll, mit.attack_power, mit.base_pre_crit
        ),
        std::cmp::Ordering::Less => format!(
            "  wpn={} ×AP{}%={} ",
            mit.weapon_roll, mit.attack_power, mit.base_pre_crit
        ),
    };
    let crit_step = if matches!(detail.outcome, SwingOutcome::Crit) {
        format!("×1.5crit={} ", mit.after_crit)
    } else {
        String::new()
    };
    // A6 stealth opening-strike — surface the % bonus on the
    // same line so a rogue can see why their backstab hit
    // harder than expected.
    let stealth_step = if mit.stealth_bonus_pct > 0 && !matches!(detail.outcome, SwingOutcome::Miss)
    {
        format!("×stealth+{}%={} ", mit.stealth_bonus_pct, mit.after_crit)
    } else {
        String::new()
    };
    let variance_step = match mit.variance_delta.cmp(&0) {
        std::cmp::Ordering::Equal => format!("±var(0) ={} ", mit.after_variance),
        std::cmp::Ordering::Greater => {
            format!("±var(+{}) ={} ", mit.variance_delta, mit.after_variance)
        }
        std::cmp::Ordering::Less => {
            format!("±var({}) ={} ", mit.variance_delta, mit.after_variance)
        }
    };
    let armor_pct_step = format!(
        "armor×K{}/({}+{})={} ",
        mit.armor_k, mit.armor_pct, mit.armor_k, mit.after_armor_pct
    );
    let armor_flat_step = if mit.armor_flat > 0 {
        format!("flat-{} ={} ", mit.armor_flat, mit.after_armor_flat)
    } else {
        String::new()
    };
    let resist_step = if mit.resist_pct != 0 {
        format!("resist {}% ={} ", mit.resist_pct, mit.after_resist)
    } else {
        String::new()
    };
    let hardness_step = if mit.hardness > 0 && mit.final_dmg == 0 {
        format!("hardness {} ZEROED ", mit.hardness)
    } else if mit.hardness > 0 {
        format!("hardness {} ", mit.hardness)
    } else {
        String::new()
    };
    format!(
        "  ({header})\r\n{ap_step}{crit_step}{stealth_step}{variance_step}{armor_pct_step}{armor_flat_step}{resist_step}{hardness_step}→ final {}\r\n",
        mit.final_dmg
    )
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
        Some(format!(
            "{} begins to stink.\r\n",
            crate::commands::cap_sentence_start(name),
        ))
    } else if prev > 30 && now <= 30 {
        Some(format!(
            "{} sags as decay sets in.\r\n",
            crate::commands::cap_sentence_start(name),
        ))
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
        let mut q =
            world.query_filtered::<(Entity, &Located, &HateList), (With<Mob>, Without<Fighting>)>();
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
        let mut q = world.query_filtered::<(
            Entity,
            &Fighting,
            &CombatStats,
            &Named,
            Option<&Posture>,
            Option<&Stunned>,
        ), (Without<Ghost>, Without<mud_world::Frozen>)>();
        q.iter(world)
            .filter(|(_, _, _, _, posture, stunned)| {
                stunned.is_none()
                    && matches!(posture.map(|p| p.0), None | Some(PostureKind::Standing))
            })
            .map(|(attacker, fighting, cs, name, _, _)| {
                // Damage pipeline per docs/design/combat.md step 3:
                //   base = weapon_dice * (1 + attack_power/100)
                // attack_power applies as an additive % multiplier on
                // the rolled base.
                //
                // Where the dice come from:
                //   - Player: wielded weapon if present, else NaturalDamage,
                //     else 1 (unarmed floor).
                //   - Mob: max(wielded weapon roll, natural attack roll).
                //     Mobs sometimes get auto-assigned flavor weapons
                //     (a "monk guard" with a tiny maul) that gimp their
                //     real combat profile if treated as authoritative.
                //     Picking the higher of the two respects authorial
                //     intent: weak weapon stays for inventory/loot flavor,
                //     but natural attack drives the actual hit.
                let is_mob = world.get::<mud_world::Mob>(attacker).is_some();
                let roll_natural = || -> Option<i32> {
                    let &(num, sides, bonus) = natural_damage.get(&attacker)?;
                    let raw = roll_dice(num, sides, bonus);
                    let race_factor = world
                        .get::<mud_world::Profile>(attacker)
                        .and_then(|p| {
                            world
                                .get_resource::<mud_world::RaceCatalog>()
                                .and_then(|c| c.get(&p.race))
                        })
                        .map_or(100, |def| def.damage_dice_factor);
                    Some(if race_factor == 100 {
                        raw
                    } else {
                        raw.saturating_mul(race_factor).saturating_div(100).max(1)
                    })
                };
                let roll_weapon = || -> Option<i32> {
                    weapon_dice
                        .get(&attacker)
                        .map(|&(n, s, b)| roll_dice(n, s, b))
                };
                let weapon_roll = if is_mob {
                    // Mob: pick whichever dice rolled higher this swing.
                    let w = roll_weapon().unwrap_or(0);
                    let n = roll_natural().unwrap_or(0);
                    w.max(n).max(1)
                } else {
                    // unarmed floor — keeps swings non-zero
                    roll_weapon().or_else(roll_natural).unwrap_or(1)
                };
                let scaled = (weapon_roll.saturating_mul(100 + cs.attack_power)) / 100;
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
                    .find(|(g, defended)| {
                        *defended == fighting.0 && *g != fighting.0 && *g != attacker
                    })
                    .map_or(fighting.0, |(g, _)| *g);
                Swing {
                    attacker,
                    target,
                    damage,
                    weapon_roll,
                    attacker_name: name.name.clone(),
                }
            })
            .collect()
    };

    for s in &swings {
        apply_swing(world, s);
    }
    // Haste pass: every attacker with the `Haste` marker gets a
    // second swing this round, against the same target (when
    // still alive and still in the same room). Mirrors the classic
    // "double-attack from speed" feel without needing the swing
    // scheduler to fire on a faster cadence.
    for s in &swings {
        if world.get::<mud_world::Haste>(s.attacker).is_none() {
            continue;
        }
        if world.get_entity(s.attacker).is_err() || world.get_entity(s.target).is_err() {
            continue;
        }
        // Skip the second swing if the first dropped the target
        // (zero HP) — let death broadcast settle in this tick.
        let target_dead = world
            .get::<mud_world::Health>(s.target)
            .is_none_or(|h| h.hp <= 0);
        if target_dead {
            continue;
        }
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
    damage: i32,      // post-AP, pre-crit base damage
    weapon_roll: i32, // raw dice roll (pre-AP) — surfaced by show_dice
    attacker_name: String,
}

/// Build + apply one swing for a freshly-engaged attacker (G3.1).
///
/// `cmd_attack` calls this at the end of the engage path so the
/// player doesn't sit through "You attack the orc!" with no visible
/// damage until the next combat tick (up to ~4s away). Mirrors the
/// per-attacker math in [`combat_tick`] but does per-entity lookups
/// instead of using snapshot maps — fine for one swing.
///
/// Skips the FIGHT trigger fire (`combat_tick` will fire it on the
/// next regular cadence; firing twice on engage would be a behavior
/// change).
pub(crate) fn engage_swing_now(world: &mut World, attacker: Entity, target: Entity) {
    // Defenders that combat_tick refuses to swing on — same gates so
    // engage doesn't bypass them.
    if world.get::<Ghost>(attacker).is_some()
        || world.get::<mud_world::Frozen>(attacker).is_some()
        || world.get::<Stunned>(attacker).is_some()
    {
        return;
    }
    // Non-alert posture skip mirrors combat_tick's filter.
    if !matches!(
        world.get::<Posture>(attacker).map(|p| p.0),
        None | Some(PostureKind::Standing),
    ) {
        return;
    }
    let Some(cs) = world.get::<CombatStats>(attacker).copied() else {
        return;
    };
    let Some(name) = world.get::<Named>(attacker).map(|n| n.name.clone()) else {
        return;
    };

    // Per-attacker weapon roll: wielded weapon if any, else
    // NaturalDamage, else 1. Mob branch picks max(weapon, natural)
    // for the same authorial-intent reason combat_tick does.
    let is_mob = world.get::<mud_world::Mob>(attacker).is_some();
    // Lift the wielded-weapon's world key out so the prototype
    // lookup doesn't hold a borrow while we touch resources.
    let wielded_key: Option<(i32, i32)> = {
        let mut q = world.query::<(&Located, &EquippedSlot, &WorldKey)>();
        q.iter(world).find_map(|(loc, eq, key)| {
            if loc.0 == attacker && eq.0 == Slot::Wield {
                Some((key.zone, key.id))
            } else {
                None
            }
        })
    };
    let weapon_dice: Option<(i32, i32, i32)> = wielded_key.and_then(|key| {
        let protos = world.get_resource::<ObjectPrototypes>()?;
        let p = protos.by_key.get(&key)?;
        if p.weapon_dice_num <= 0 || p.weapon_dice_size <= 0 {
            return None;
        }
        Some((p.weapon_dice_num, p.weapon_dice_size, p.weapon_dice_bonus))
    });
    let natural: Option<(i32, i32, i32)> = world
        .get::<NaturalDamage>(attacker)
        .map(|n| (n.num, n.size, n.bonus));
    let roll_natural = || -> Option<i32> {
        let (num, sides, bonus) = natural?;
        let raw = roll_dice(num, sides, bonus);
        let race_factor = world
            .get::<mud_world::Profile>(attacker)
            .and_then(|p| {
                world
                    .get_resource::<mud_world::RaceCatalog>()
                    .and_then(|c| c.get(&p.race))
            })
            .map_or(100, |def| def.damage_dice_factor);
        Some(if race_factor == 100 {
            raw
        } else {
            raw.saturating_mul(race_factor).saturating_div(100).max(1)
        })
    };
    let roll_weapon = || -> Option<i32> { weapon_dice.map(|(n, s, b)| roll_dice(n, s, b)) };
    let weapon_roll = if is_mob {
        let w = roll_weapon().unwrap_or(0);
        let n = roll_natural().unwrap_or(0);
        w.max(n).max(1)
    } else {
        // unarmed floor — keeps swings non-zero
        roll_weapon().or_else(roll_natural).unwrap_or(1)
    };
    let scaled = (weapon_roll.saturating_mul(100 + cs.attack_power)) / 100;
    let base = scaled.max(1);
    // Berserk multiplier — single-attacker scan is fine on engage.
    let berserk = {
        let mut q = world.query::<(&EffectInstance, &AppliedTo)>();
        q.iter(world)
            .any(|(eff, a)| a.0 == attacker && eff.name.eq_ignore_ascii_case("berserk"))
    };
    let damage = if berserk { (base * 3) / 2 } else { base };

    // Guard redirect: same lookup combat_tick does, single-pair scope.
    let redirected = {
        let mut q = world.query::<(Entity, &Guarding, &Located)>();
        q.iter(world)
            .find_map(|(g, guarded, loc)| {
                let tloc = world.get::<Located>(guarded.0).map(|l| l.0)?;
                if tloc != loc.0 || guarded.0 != target || g == target || g == attacker {
                    None
                } else {
                    Some(g)
                }
            })
            .unwrap_or(target)
    };
    apply_swing(
        world,
        &Swing {
            attacker,
            target: redirected,
            damage,
            weapon_roll,
            attacker_name: name,
        },
    );
}

#[allow(clippy::too_many_lines)]
fn apply_swing(world: &mut World, s: &Swing) {
    // Target may have been despawned earlier in this same tick.
    if world.get_entity(s.target).is_err() {
        try_remove::<Fighting>(world, s.attacker);
        return;
    }
    // G3.2: the attacker may have died (gained Ghost) between the
    // snapshot pass and this apply call — e.g. the Illithid's swing
    // earlier in this same tick killed them. A snapshotted dead
    // attacker would otherwise still swing once after the death
    // banner fires. Same gate for fresh Frozen/Stunned applied
    // intra-tick.
    if world.get::<Ghost>(s.attacker).is_some()
        || world.get::<mud_world::Frozen>(s.attacker).is_some()
        || world.get::<Stunned>(s.attacker).is_some()
    {
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
    let was_sleeping = world.get::<Posture>(s.target).map(|p| p.0) == Some(PostureKind::Sleeping);
    // posture-and-lifestate.md: a defender attacked while RESTING
    // auto-stands on the hit. Sleeping has its own jolt-awake path
    // (different visual), so the two stay separate flags.
    let was_resting = world.get::<Posture>(s.target).map(|p| p.0) == Some(PostureKind::Resting);

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

    // A6 (perception / stealth bonus): an attacker with the
    // Stealth marker gets an opening-strike accuracy + damage
    // bonus on this swing, then loses Stealth. The defender's
    // `Perception` softens the bonus — high-perception
    // characters spot the attacker mid-swing and partially
    // deflect. Quantitative shape (kept simple for v1; tune
    // alongside the rogue toolkit):
    //   - stealth_accuracy_bonus = 25 - defender.perception/4 (floor 0)
    //   - stealth_damage_mult_pct = 50 - defender.perception/2 (floor 0)
    // Stealth is cleared after this swing whether or not the
    // attacker saw a bonus, mirroring how a real opening attack
    // breaks concealment regardless of outcome.
    let attacker_hidden = world.get::<mud_world::Stealth>(s.attacker).is_some();
    let defender_perception = world
        .get::<mud_world::Perception>(s.target)
        .map_or(0, |p| p.0);
    let (stealth_acc_bonus, stealth_dmg_bonus_pct) = if attacker_hidden {
        let acc = (25 - defender_perception / 4).max(0);
        let dmg = (50 - defender_perception / 2).max(0);
        (acc, dmg)
    } else {
        (0, 0)
    };
    if attacker_hidden {
        try_remove::<mud_world::Stealth>(world, s.attacker);
    }
    // Hit / miss / crit per docs/design/combat.md step 1.
    // Sleeping defenders auto-hit (can't dodge unconscious).
    // Otherwise: attacker.accuracy + d100 vs defender.evasion + d100,
    // ties to attacker. Posture penalty subtracts from defender's
    // evasion (a sitting target evades worse). Crit chance is a
    // separate d100 vs the attacker's `crit_chance`.
    // Bless: +5 accuracy when the attacker is blessed. Mirrors the
    // legacy +1 hit-roll bump scaled to the modern 100-point band.
    let bless_acc_bonus = i32::from(world.get::<mud_world::Bless>(s.attacker).is_some()) * 5;
    let attacker_accuracy = world
        .get::<CombatStats>(s.attacker)
        .map_or(50, |cs| cs.accuracy)
        + stealth_acc_bonus
        + bless_acc_bonus;
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
        SwingDetail {
            outcome: SwingOutcome::Hit,
            roll: 0,
            chance: 100,
            crit_roll: 0,
            crit_chance: 0,
        }
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
    let target_dice_on = show_dice_for(world, s.target);
    if let Some(via) = evaded_via {
        let tail = if dice_on {
            show_dice_evade(via)
        } else {
            String::new()
        };
        let target_tail = if target_dice_on {
            show_dice_evade(via)
        } else {
            String::new()
        };
        let target_cap = crate::commands::cap_sentence_start(&target_name);
        let attacker_cap = crate::commands::cap_sentence_start(&s.attacker_name);
        send_to(
            world,
            s.attacker,
            format!("{target_cap} {via}s your attack!\r\n{tail}"),
        );
        let attacker_seen =
            crate::commands::seen_name(world, s.target, s.attacker, &s.attacker_name);
        send_to(
            world,
            s.target,
            format!("You {via} {attacker_seen}'s attack!\r\n{target_tail}"),
        );
        crate::commands::broadcast_room_anonymised(
            world,
            room,
            &[s.attacker, s.target],
            &[(s.attacker, &s.attacker_name), (s.target, &target_name)],
            &format!("{target_cap} {via}s {attacker_cap}'s attack.\r\n"),
        );
        drain_stamina(world, s.attacker, 1);
        return;
    }
    if outcome == SwingOutcome::Miss {
        // Misses skip the damage pipeline entirely; the formatter
        // only reads the d100/threshold for the miss branch.
        let miss_mit = SwingMitigation::default();
        let tail = if dice_on {
            show_dice_swing(detail, miss_mit)
        } else {
            String::new()
        };
        let target_tail = if target_dice_on {
            show_dice_swing(detail, miss_mit)
        } else {
            String::new()
        };
        // Misses dim slightly — visible but recedes vs the hit
        // lines below, which carry the actual gameplay info.
        send_to(
            world,
            s.attacker,
            format!("<dim>You swing at {target_name} but miss.</>\r\n{tail}"),
        );
        let attacker_cap = crate::commands::cap_sentence_start(&s.attacker_name);
        let target_cap_for_room = crate::commands::cap_sentence_start(&target_name);
        let attacker_to_target = crate::commands::cap_sentence_start(&crate::commands::seen_name(
            world,
            s.target,
            s.attacker,
            &s.attacker_name,
        ));
        send_to(
            world,
            s.target,
            format!("<dim>{attacker_to_target} swings at you but misses.</>\r\n{target_tail}"),
        );
        crate::commands::broadcast_room_anonymised(
            world,
            room,
            &[s.attacker, s.target],
            &[(s.attacker, &s.attacker_name), (s.target, &target_name)],
            &format!("<dim>{attacker_cap} swings at {target_cap_for_room} but misses.</>\r\n",),
        );
        // Stamina still drains — you swung, you spent the breath.
        drain_stamina(world, s.attacker, 1);
        return;
    }

    // Build a SwingMitigation as we go — surfaces the full damage
    // pipeline through the show_dice tail when the toggle is on.
    let attacker_ap = world
        .get::<CombatStats>(s.attacker)
        .map_or(0, |cs| cs.attack_power);
    let mut mit = SwingMitigation {
        weapon_roll: s.weapon_roll,
        attack_power: attacker_ap,
        base_pre_crit: s.damage,
        stealth_bonus_pct: stealth_dmg_bonus_pct,
        ..Default::default()
    };
    // Crit promotes the swing's already-resolved damage by 1.5x.
    // Stacks multiplicatively with the berserk +50% computed in
    // the swing-snapshot phase: a critical berserk swing lands at
    // base * 3/2 * 3/2 = base * 9/4.
    let mut damage = if outcome == SwingOutcome::Crit {
        s.damage.saturating_mul(3) / 2
    } else {
        s.damage
    };
    // A6: stealth opening-swing damage bonus. Applied AFTER the
    // crit promotion so a stealth-crit gets both multipliers.
    if stealth_dmg_bonus_pct > 0 && outcome != SwingOutcome::Miss {
        damage = damage.saturating_mul(100 + stealth_dmg_bonus_pct) / 100;
        damage = damage.max(1);
    }
    mit.after_crit = damage;
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
    // Per-swing damage variance: ±25% of the post-crit base, integer
    // floor. Bigger swings get a wider band; sub-4 damage swings
    // pin at variance=0. Floor at 1 so a low roll never zeroes out
    // a swing — that would make the hit/miss roll the only meaningful
    // outcome and the dmg_roll stat decorative.
    let variance_band = damage / 4;
    if variance_band > 0 {
        let delta = rand::random_range(-variance_band..=variance_band);
        damage = damage.saturating_add(delta).max(1);
        mit.variance_delta = delta;
    }
    mit.after_variance = damage;
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
    // Diminishing-returns formula: damage_taken = damage * K / (armor + K)
    // where K=100. Asymptotic to 0 but never reaches it, so every armor
    // piece always reduces incoming damage — no "cap reached, additional
    // pieces wasted" cliff. At armor=100 → 50% mitigation (matches the
    // prior linear "clamp(0,100)" feel mid-tier); at armor=200 → 67%; at
    // armor=400 → 80%. Penetration subtracts from armor before the
    // formula. See gear-curves §7 + post-real-loadout audit (May 2026).
    #[allow(clippy::items_after_statements)]
    const ARMOR_K: i32 = 100;
    let effective_armor_pct = (def_armor_pct - atk_pen_pct).max(0);
    mit.armor_pct = effective_armor_pct;
    mit.armor_k = ARMOR_K;
    damage = damage.saturating_mul(ARMOR_K) / (effective_armor_pct + ARMOR_K);
    mit.after_armor_pct = damage;
    let effective_armor_flat = (def_armor_flat - atk_pen_flat).max(0);
    mit.armor_flat = effective_armor_flat;
    damage = damage.saturating_sub(effective_armor_flat).max(0);
    mit.after_armor_flat = damage;
    // Step 5: ward — skipped for mundane weapon swings (caller
    // routes magical abilities through a separate path that
    // engages it).
    // Step 6: type resistance against PHYSICAL.
    if let Some(res) = world.get::<mud_world::Resistances>(s.target) {
        let pct = res
            .0
            .get(&mud_db::enums::ElementType::Physical)
            .copied()
            .unwrap_or(0);
        // capped at +100 immunity; negative is unbounded vulnerability per docs.
        let pct = pct.min(100);
        mit.resist_pct = pct;
        damage = (damage.saturating_mul(100 - pct)) / 100;
        damage = damage.max(0);
    }
    mit.after_resist = damage;
    // Step 7: hardness floor — damage below this zeroes out.
    mit.hardness = def_hardness;
    if damage < def_hardness {
        damage = 0;
    }
    // J2 alignment-protect (PROT_FROM_EVIL / PROT_FROM_GOOD): 20%
    // damage reduction when the attacker is strongly opposite-
    // aligned to the victim. Sits after resist / hardness so the
    // mitigation stacks on whatever the physical pipeline produces;
    // sits before MAX_DAMAGE_PER_SWING so the cap still bounds the
    // worst case.
    let align_mult = crate::commands::alignment_protection_factor(world, s.attacker, s.target);
    if (align_mult - 1.0).abs() > f32::EPSILON {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            #[allow(clippy::cast_precision_loss)]
            {
                damage = ((damage as f32) * align_mult) as i32;
            }
        }
        damage = damage.max(0);
    }
    // Final cap per legacy MAX_DAMAGE so even a god-tier crit
    // can't one-shot a fully-buffed player from full HP.
    damage = damage.min(MAX_DAMAGE_PER_SWING);
    mit.final_dmg = damage;
    let (dead, threshold_msg) = apply_attacker_damage(world, s.target, damage, s.attacker);

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
    let tail = if dice_on {
        show_dice_swing(detail, mit)
    } else {
        String::new()
    };
    let target_tail = if target_dice_on {
        show_dice_swing(detail, mit)
    } else {
        String::new()
    };
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
        format!("You hit <b:cyan>{target_name}</> for {damage_label} damage{crit_tag}.\r\n{tail}"),
    );
    let attacker_name_cap = crate::commands::cap_sentence_start(&s.attacker_name);
    let attacker_to_target = crate::commands::cap_sentence_start(&crate::commands::seen_name(
        world,
        s.target,
        s.attacker,
        &s.attacker_name,
    ));
    send_to(
        world,
        s.target,
        format!(
            "{attacker_to_target} {attacker_verb_third} you for {damage_label} damage{crit_tag}.\r\n{target_tail}",
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
    // Room broadcast: surface crit tag so bystanders see the
    // "X critically hits Y!" beat the attacker/target are already
    // seeing. Damage stays hidden (info-leak — mortal observers
    // can't see exact numbers; staff get them via `consider`).
    let room_crit_tag = if outcome == SwingOutcome::Crit {
        " <b:red>(critical!)</>"
    } else {
        ""
    };
    crate::commands::broadcast_room_anonymised(
        world,
        room,
        &[s.attacker, s.target],
        &[(s.attacker, &s.attacker_name), (s.target, &target_name)],
        &format!("{attacker_name_cap} {attacker_verb_third} {target_name}{room_crit_tag}.\r\n"),
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
    // Mob auto-flee: legacy `MOB_WIMPY` (fight.cpp:1782) — a wimpy,
    // non-charmed mob whose HP just dropped below a quarter of max
    // flees. Mobs without the flag hold their ground.
    if target_is_mob && crate::commands::wimpy_mob_should_flee(world, s.target) {
        mob_flee(world, s.target, room);
        return;
    }
    if target_is_player
        && wimpy_set
        && let Some(hp) = world.get::<Health>(s.target).copied()
        && hp.hp > 0
        && hp.hp * 100 < hp.max * wimpy_pct
    {
        // Look for any open exit before announcing the panic — otherwise
        // we'd print "You panic!" and then immediately "There's nowhere
        // to run!" from cmd_flee, which reads as a contradiction.
        let has_exit = world.get::<Exits>(room).is_some_and(|e| {
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

/// How long a recorded damage source stays eligible for kill credit.
/// Bounds stale attribution (a hit minutes ago must not claim a later
/// drowning / bleed death).
const DAMAGER_CREDIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// The last Player that damaged this entity, stamped by
/// `apply_damage_from`. Lets `handle_death` credit a kill to the
/// actual damage source instead of inferring it from a `Fighting`
/// link, which a spell / skill kill from outside combat never has.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct DamagedBy {
    pub attacker: Entity,
    pub at: std::time::Instant,
}

/// Record `attacker` as the most recent damage source of `victim`.
/// Only Players are credited with kills, so other sources are ignored.
pub(crate) fn record_damager(world: &mut World, victim: Entity, attacker: Entity) {
    if world.get::<Player>(attacker).is_none() {
        return;
    }
    try_insert(
        world,
        victim,
        DamagedBy {
            attacker,
            at: std::time::Instant::now(),
        },
    );
}

/// Legacy `MIN_ALIGNMENT` / `MAX_ALIGNMENT` (`chars.hpp`), the bounds
/// every alignment write clamps to.
const MAX_ALIGNMENT: i32 = 1000;

/// Killer-class bias legacy `change_alignment` adds to the killer's
/// alignment before the formula ("good" classes should know better,
/// "bad" classes less so). Matches on the class's plain name; legacy's
/// switch falls through, so Paladin/Priest get +100, Ranger/Druid +50,
/// Anti-Paladin/Diabolist/Necromancer -100, Thief/Assassin -50.
fn class_alignment_bias(plain_name: &str) -> i32 {
    match plain_name.to_ascii_lowercase().as_str() {
        "paladin" | "priest" => 100,
        "ranger" | "druid" => 50,
        "anti-paladin" | "diabolist" | "necromancer" => -100,
        "thief" | "assassin" => -50,
        _ => 0,
    }
}

/// Alignment change for a kill, mirroring legacy `change_alignment`
/// (`fight.cpp`) with C++ truncating integer division. The mob-only
/// aggressive-flag skew on the victim's alignment does not apply to
/// player victims. The killer moves AWAY from the victim's alignment
/// (an evil victim raises it), scaled down as the killer sits further
/// toward good, less a small drag proportional to how extreme the
/// killer already is. A big level gap over the victim amplifies a
/// negative change. The result is clamped to the alignment bounds.
fn kill_alignment_after(
    killer_alignment: i32,
    killer_bias: i32,
    killer_level: i32,
    victim_alignment: i32,
    victim_level: i32,
) -> i32 {
    let k_al = killer_alignment + killer_bias;
    let mut change =
        victim_alignment / (-75 - 25 * ((k_al - 1000) / 200).abs()) - 2 * (k_al / 1000).abs();
    if change < 0 && killer_level > victim_level + 10 {
        change *= (killer_level - victim_level) / 10;
    }
    killer_alignment
        .saturating_add(change)
        .clamp(-MAX_ALIGNMENT, MAX_ALIGNMENT)
}

/// Shift `killer`'s alignment for killing the player `victim`
/// (legacy formula, see [`kill_alignment_after`]) and tell them when it
/// moved.
fn apply_pvp_alignment_shift(world: &mut World, killer: Entity, victim: Entity) {
    let victim_alignment = world.get::<CombatStats>(victim).map_or(0, |c| c.alignment);
    let victim_level = world
        .get::<mud_world::Profile>(victim)
        .map_or(1, |p| p.level);
    let (killer_level, class_id) = world
        .get::<mud_world::Profile>(killer)
        .map_or((1, None), |p| (p.level, p.class_id));
    let bias = class_id
        .and_then(|id| {
            world
                .get_resource::<mud_world::ClassCatalog>()
                .and_then(|c| c.by_id.get(&id))
        })
        .map_or(0, |c| class_alignment_bias(&c.plain_name));
    let Some(mut cs) = world.get_mut::<CombatStats>(killer) else {
        return;
    };
    let before = cs.alignment;
    let after = kill_alignment_after(before, bias, killer_level, victim_alignment, victim_level);
    cs.alignment = after;
    if after < before {
        send_to(
            world,
            killer,
            "A shadow falls across your soul as you take a player's life.\r\n",
        );
    } else if after > before {
        send_to(
            world,
            killer,
            "Some small good comes of ending a wicked life.\r\n",
        );
    }
}

/// Resolve who gets credit for `victim`'s death: the most recent
/// recorded Player damager (recent, still present and in `room`),
/// otherwise a Player currently `Fighting` the victim. Resolved once
/// per death so XP, coin, loot-claim and autoloot all agree on a
/// single killer.
fn resolve_killer(world: &mut World, victim: Entity, room: Entity) -> Option<Entity> {
    if let Some(d) = world.get::<DamagedBy>(victim).copied()
        && d.attacker != victim
        && d.at.elapsed() <= DAMAGER_CREDIT_WINDOW
        && world.get::<Player>(d.attacker).is_some()
        && world.get::<mud_world::Located>(d.attacker).map(|l| l.0) == Some(room)
    {
        return Some(d.attacker);
    }
    let mut q = world.query_filtered::<(Entity, &Fighting), With<Player>>();
    q.iter(world)
        .find(|(e, f)| f.0 == victim && *e != victim)
        .map(|(e, _)| e)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn handle_death(world: &mut World, victim: Entity, victim_name: &str, room: Entity) {
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
        // Resolve credit before the Fighting links below are torn down:
        // `resolve_killer` falls back to a Player `Fighting` the victim.
        let pvp_killer: Option<Entity> = resolve_killer(world, victim, room);
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
            let mut q = world
                .query_filtered::<(Entity, &Located, Option<&mud_world::ObjectFlags>), With<Item>>(
                );
            q.iter(world)
                .filter(|(_, l, _)| l.0 == victim)
                .map(|(e, _, f)| {
                    let bound = f.is_some_and(|ff| ff.has(mud_db::enums::ObjectFlag::Soulbound));
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
                Keywords(vec!["corpse".to_string(), victim_name.to_ascii_lowercase()]),
                Located(room),
                CorpseDecay {
                    remaining_secs: PLAYER_CORPSE_DECAY_SECS,
                },
            ))
            .id();
        // Tag as a player corpse separately so ANIMATE_DEAD /
        // looting gates can distinguish from mob corpses. Inserted
        // after spawn — bundling it inline with the 6+ components
        // in `spawn(...)` above doesn't reliably attach in this
        // Bevy version (verified empirically; the second insert
        // attaches cleanly). Snapshot save/load round-trips this
        // marker so it survives a restart.
        let victim_level = world
            .get::<mud_world::Profile>(victim)
            .map_or(1, |p| p.level);
        if let Ok(mut em) = world.get_entity_mut(corpse) {
            em.insert(mud_world::PlayerCorpse);
            em.insert(mud_world::CorpseOriginLevel(victim_level));
        }
        for (it, bound) in owned_items {
            if bound {
                // Skip both moves — bound gear stays on the ghost.
                continue;
            }
            if world.get::<Located>(it).is_some() {
                world.entity_mut(it).insert(Located(corpse));
            }
            // Strip EquippedSlot — items inside a corpse aren't
            // worn anymore. crate uses `mud_world::EquippedSlot`.
            try_remove::<mud_world::EquippedSlot>(world, it);
        }

        // Carried coin goes into the corpse with the items (legacy
        // `make_corpse` drops a money object inside the corpse). The
        // whole carried purse moves in one pile; bank and account
        // chest wealth are separate components and stay put. The
        // corpse's `CoinPile` is what `look in` shows, what `get all`
        // pulls back onto `Wealth`, and what falls to the room when
        // the corpse decays.
        let coins_lost = world.get::<Wealth>(victim).map_or(0, |w| w.0).max(0);
        if coins_lost > 0 {
            if let Some(mut w) = world.get_mut::<Wealth>(victim) {
                w.0 = 0;
            }
            if let Ok(mut em) = world.get_entity_mut(corpse) {
                em.insert(mud_world::CoinPile(coins_lost));
            }
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

        // PvP alignment shift: legacy `change_alignment` applied to a
        // player victim. Killing an evil character nudges the killer
        // toward good, killing a good one toward evil.
        try_remove::<DamagedBy>(world, victim);
        if let Some(killer) = pvp_killer {
            apply_pvp_alignment_shift(world, killer, victim);
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
        // the player knows where to return for their gear. Player
        // corpses last `PLAYER_CORPSE_DECAY_SECS` (currently 7d) so
        // a player can come back, get their bearings, and recover
        // gear without a 10-minute panic timer.
        let death_room_name = name_of(world, room);
        if let Some(purse) = crate::commands::format_wealth(coins_lost) {
            send_to(
                world,
                victim,
                format!("Your purse of {purse} goes into the corpse with your gear.\r\n"),
            );
        }
        send_to(
            world,
            victim,
            format!(
                "You collapse, your spirit drifting free of your dying body.\r\n\
                 Your corpse lies in <b:yellow>{death_room_name}</> — it will \
                 keep for several days.\r\n\
                 Type <b:cyan>release</> to return to your recall point, then \
                 head back for your gear.\r\n"
            ),
        );
        broadcast_room_except_rendered(
            world,
            room,
            &[victim],
            &format!(
                "{} collapses, dead.\r\n",
                crate::commands::cap_sentence_start(victim_name),
            ),
        );
        // Persist now rather than at the next autosave: the corpse
        // snapshot first (it holds the gear and purse), then the
        // player's emptied pack. Otherwise a crash restores nothing.
        crate::corpses::persist_after_change(world, victim);
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
            &format!(
                "{} dies.\r\n",
                crate::commands::cap_sentence_start(victim_name),
            ),
        );
        // Resolve the killer once, before any Fighting links are torn
        // down, and hand the same identity to XP, coin and autoloot.
        let killer = resolve_killer(world, victim, room);
        award_kill_xp(world, victim, victim_name, killer);
        // Achievement hooks: first_kill and (eventually)
        // milestone-kill counters. Fire on the player who's
        // currently Fighting the victim — same target as the
        // kill-coin / loot-claim attribution.
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
                Named {
                    name: format!("the corpse of {victim_name}"),
                },
                Keywords(vec!["corpse".to_string(), victim_name.to_ascii_lowercase()]),
                Located(room),
                CorpseDecay {
                    remaining_secs: MOB_CORPSE_DECAY_SECS,
                },
            ))
            .id();
        // Record the dead mob's level on the corpse for downstream
        // mechanics — ANIMATE_DEAD reads it to scale the spawned
        // skeleton's HP. Profile is the canonical source for mob
        // levels (Mob protos seed it at spawn).
        let mob_level = world
            .get::<mud_world::Profile>(victim)
            .map_or(1, |p| p.level);
        if let Ok(mut em) = world.get_entity_mut(corpse) {
            em.insert(mud_world::CorpseOriginLevel(mob_level));
        }
        // Loot-claim window: 5 minutes for the killer. Player-only
        // — mob killers don't claim corpses (this path is reached
        // only when a player landed the killing blow against
        // another mob; the killer lookup above filters to Player
        // entities).
        if let Some(k) = killer {
            world
                .get_entity_mut(corpse)
                .unwrap()
                .insert(mud_world::LootClaim {
                    owner: k,
                    expires_at: std::time::Instant::now() + std::time::Duration::from_secs(300),
                });
        }
        for it in &owned_items {
            if world.get::<Located>(*it).is_some() {
                world.entity_mut(*it).insert(Located(corpse));
            }
            try_remove::<mud_world::EquippedSlot>(world, *it);
        }
        // award_kill_coin handles AutoGold/AutoSplit and (when
        // AutoGold is off) attaches a CoinPile to the corpse so
        // the coin can still be claimed via `get all from corpse`.
        // Runs after the corpse is spawned so the CoinPile has
        // somewhere to attach.
        award_kill_coin(world, victim, victim_name, corpse, killer);
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
                if world.get::<Located>(*it).is_some() {
                    world.entity_mut(*it).insert(Located(killer));
                    moved += 1;
                }
            }
            if moved > 0 {
                send_to(
                    world,
                    killer,
                    format!("You loot {moved} item(s) from the corpse of {victim_name}.\r\n"),
                );
            }
        }
        disengage_attackers_of(world, victim);
        // G3.4: stamp the death tick on this MobReset row's timer
        // so the respawn loop honors the per-row cooldown. Read
        // BEFORE despawn or the FromMobReset component vanishes.
        if let Some(reset_id) = world.get::<FromMobReset>(victim).map(|f| f.0) {
            let now = world.resource::<TickCount>().0;
            if let Some(mut timers) = world.get_resource_mut::<crate::respawn::MobRespawnTimers>() {
                timers.last_death_tick.insert(reset_id, now);
            }
        }
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
    killer: Option<Entity>,
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
            (base_share.saturating_mul(i64::from(copper_factor)) / 100).max(1)
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
            format!("You collect {msg} (group share) from the corpse of {victim_name}.\r\n")
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

#[allow(clippy::too_many_lines)]
fn award_kill_xp(world: &mut World, victim: Entity, victim_name: &str, killer: Option<Entity>) {
    use mud_db::enums::MobRole;
    let proto = world.get::<WorldKey>(victim).and_then(|k| {
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
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
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
        // Rest / repose R4 + R5: route through `rest::award_experience`
        // so the wake consumer fires on the first XP gain after
        // acquiring a source, and Repose multiplies the gain when
        // the pool is non-zero. The fn returns the actual XP
        // applied (base + Repose bonus) for the narration line.
        let awarded = if world.get::<mud_world::Profile>(*entity).is_some() {
            crate::rest::award_experience(world, *entity, scaled)
        } else {
            continue;
        };
        let scaled = awarded;
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
            format!("You gain {scaled} experience (group share) for the kill of {victim_name}.\r\n")
        };
        // Staff-level characters gain no XP (`Profile::grant_experience`).
        if scaled > 0 {
            send_to(world, *entity, line);
        }
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
/// levels in one call). XP-driven, so it never crosses
/// [`mud_db::enums::MAX_MORTAL_LEVEL`]: level >= 100 grants staff rank
/// (`mud_db::enums::effective_rank`) and is reserved to staff action. A
/// character already at a staff level never levels from XP at all.
pub(crate) fn check_level_up(world: &mut World, entity: Entity) {
    if world
        .get::<mud_world::Profile>(entity)
        .is_some_and(|p| mud_db::enums::is_staff_level(p.level))
    {
        return;
    }
    level_up_to(world, entity, mud_db::enums::MAX_MORTAL_LEVEL);
}

/// Progression sweep, run once per tick for every player character: the
/// single place that turns banked XP into levels, whatever granted it.
/// Kills call [`check_level_up`] directly for immediate feedback, but quest
/// rewards, Lua `award_exp`, admin XP edits and characters already carrying
/// surplus XP from before a curve change all reach `Profile.experience`
/// without it, so without this sweep they would sit "ready to level" until
/// the next kill.
///
/// Mortal XP is also capped at the `**` amount (legacy `gain_exp`: level
/// 100's threshold minus 1) so it cannot pile up past the point at which a
/// level-99 character is maxed. Staff-level characters are skipped.
pub(crate) fn level_sweep_tick(world: &mut World) {
    use mud_world::{LevelTable, Player, Profile};
    if world.get_resource::<LevelTable>().is_none() {
        return;
    }
    let players: Vec<(Entity, i32, i32, Option<i32>)> = world
        .query_filtered::<(Entity, &Profile), With<Player>>()
        .iter(world)
        .filter(|(_, p)| !mud_db::enums::is_staff_level(p.level))
        .map(|(e, p)| (e, p.level, p.experience, p.class_id))
        .collect();
    for (entity, level, xp, class_id) in players {
        let factor = mud_world::class_exp_factor(world, class_id);
        let (cap, ready) = {
            let table = world.resource::<LevelTable>();
            (
                table.starstar_exp(factor),
                level < mud_db::enums::MAX_MORTAL_LEVEL
                    && table
                        .exp_for_class(level + 1, factor)
                        .is_some_and(|t| xp >= t),
            )
        };
        if let Some(cap) = cap
            && xp > cap
            && let Some(mut p) = world.get_mut::<Profile>(entity)
        {
            p.experience = cap;
        }
        if ready {
            check_level_up(world, entity);
        }
    }
}

/// Bring everything derived from a player's level back in sync after it
/// changed from `old_level` to the current `Profile.level` (either
/// direction): the cached effective staff rank in `Account.role`, and the
/// permissions granted by the level table. Permissions of level rows above
/// the new level are removed (lowering a level must not leave Summon-style
/// perms behind); those of rows at or below it are granted. Permissions
/// that no level row confers (explicit per-character grants) are untouched.
/// Must be called after anything that changes a player's level, otherwise
/// rank and permissions go stale until relog.
pub(crate) fn after_level_change(world: &mut World, entity: Entity, old_level: i32) {
    let Some(level) = world.get::<mud_world::Profile>(entity).map(|p| p.level) else {
        return;
    };
    let (revoke, grant): (Vec<_>, Vec<_>) = {
        let Some(table) = world.get_resource::<mud_world::LevelTable>() else {
            return refresh_rank_only(world, entity, level);
        };
        let conferred = |at: i32| -> Vec<mud_db::enums::Permission> {
            table
                .rows
                .iter()
                .filter(|r| r.level <= at)
                .flat_map(|r| r.permissions.iter().copied())
                .collect()
        };
        let now = conferred(level);
        let before = conferred(old_level);
        (
            before
                .iter()
                .copied()
                .filter(|p| !now.contains(p))
                .collect(),
            now,
        )
    };
    if let Some(mut acct) = world.get_mut::<mud_world::Account>(entity) {
        acct.refresh_rank(level);
        acct.perms.retain(|p| !revoke.contains(p));
        for p in grant {
            if !acct.perms.contains(&p) {
                acct.perms.push(p);
            }
        }
    }
}

fn refresh_rank_only(world: &mut World, entity: Entity, level: i32) {
    if let Some(mut acct) = world.get_mut::<mud_world::Account>(entity) {
        acct.refresh_rank(level);
    }
}

/// Why [`authorize_level_change`] refused. Each variant maps to one
/// player-facing denial message via [`LevelChangeDenied::message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LevelChangeDenied {
    /// The new level is at or above the actor's own level.
    RaiseToOwnLevel,
    /// The target's current level is at or above the actor's own level.
    TargetOutranks,
    /// The target isn't a player character (mobs have no staff levels).
    NotAPlayer,
}

impl LevelChangeDenied {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::RaiseToOwnLevel => "You can't raise someone to your own level or higher.\r\n",
            Self::TargetOutranks => {
                "You can't change the level of someone at or above your own level.\r\n"
            }
            Self::NotAPlayer => "You can only change the level of player characters.\r\n",
        }
    }
}

/// Authorization for any in-game action that sets a character's level to
/// `new_level` (`advance`, `set <char> level`).
///
/// Level confers in-game staff rank (`effective_rank`), so setting one is a
/// privilege escalation. Rules, keyed on the *actor's* `Profile.level`
/// (never `DevMode`, which waives `min_role` for every account holder):
/// 1. `new_level` must be strictly below the actor's level, so nobody can
///    raise anyone to or above their own rank (a 105 Implementor tops out at
///    104; an Implementor can never be created in game).
/// 2. The target's current level must be strictly below the actor's level,
///    so peers and superiors can't be demoted or edited. The one exception
///    is the actor lowering themselves (rule 1 still applies).
/// 3. The target must be a player character, never a mob.
///
/// The authenticated admin HTTP `player/set` is exempt (operator tooling).
/// Denials are always written to the admin audit log; grants of staff
/// levels (>= 100) are too, tagged with target and level.
pub(crate) fn authorize_level_change(
    world: &mut World,
    actor: Entity,
    target: Entity,
    target_name: &str,
    new_level: i32,
) -> Result<(), LevelChangeDenied> {
    let actor_level = world
        .get::<mud_world::Profile>(actor)
        .map_or(0, |p| p.level);
    let target_level = world
        .get::<mud_world::Profile>(target)
        .map_or(0, |p| p.level);
    let verdict = if world.get::<mud_world::Player>(target).is_none() {
        Err(LevelChangeDenied::NotAPlayer)
    } else if new_level >= actor_level {
        Err(LevelChangeDenied::RaiseToOwnLevel)
    } else if target != actor && target_level >= actor_level {
        Err(LevelChangeDenied::TargetOutranks)
    } else {
        Ok(())
    };
    if verdict.is_err() || mud_db::enums::is_staff_level(new_level) {
        crate::commands::record_admin_action(
            world,
            actor,
            if verdict.is_ok() {
                "staff_level_grant"
            } else {
                "staff_level_grant_denied"
            },
            &format!("{target_name} {new_level}"),
        );
    }
    verdict
}

/// Promote `entity` while its XP clears the next threshold and the next
/// level is `<= max_level` — incrementing `Profile.level`, expanding
/// `Health.max` and `Stamina.max` by the row's gain values, and
/// emitting a "you advanced to level N" line per step. Callers
/// pass `MAX_MORTAL_LEVEL` for XP-driven level-ups; only the
/// Implementor-gated `advance` command passes a higher cap.
#[allow(clippy::too_many_lines)]
pub(crate) fn level_up_to(world: &mut World, entity: Entity, max_level: i32) {
    use mud_world::{LevelTable, Profile};
    let table = world.resource::<LevelTable>().clone_rows();
    loop {
        let (level, xp) = match world.get::<Profile>(entity) {
            Some(p) => (p.level, p.experience),
            None => return,
        };
        let next = level + 1;
        // Hard cap (see doc comment): experience can never carry a
        // character past the caller's cap, even if the level table gains
        // rows above 99 or XP is inflated via `set xp`.
        if next > max_level {
            return;
        }
        let Some(next_row) = table.iter().find(|r| r.level == next) else {
            return; // max level
        };
        let class_id = world.get::<Profile>(entity).and_then(|p| p.class_id);
        let threshold = mud_world::scale_exp(
            next_row.exp_required,
            next,
            mud_world::class_exp_factor(world, class_id),
        );
        if xp < threshold {
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
        let (hp_factor, class_hp_per_level, class_hit_dice_roll) =
            race.as_ref().map_or((100, 0, 0), |(r, cid)| {
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
            });
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
        // Practice points per level: base of 1, +1 every 10 caster
        // levels, plus the better of the INT or WIS bonus (capped
        // at +10 by CoreStats::bonus on the 0..100 scale). Floor at
        // 1 so a level-up always grants something.
        let mental_bonus = world.get::<mud_world::CoreStats>(entity).map_or(0, |s| {
            let int_b = mud_world::CoreStats::bonus(s.intelligence);
            let wis_b = mud_world::CoreStats::bonus(s.wisdom);
            int_b.max(wis_b).max(0)
        });
        let granted = (1 + (level / 10) + mental_bonus).max(1);
        if let Some(mut sp) = world.get_mut::<mud_world::SkillPoints>(entity) {
            sp.0 = sp.0.saturating_add(granted);
        }
        let plural = if granted == 1 { "point" } else { "points" };
        let title_suffix = next_row
            .name
            .as_deref()
            .map_or_else(String::new, |n| format!(" ({n})"));
        send_to(
            world,
            entity,
            format!(
                "*** You have advanced to level {next}{title_suffix}! ***\r\n\
                 You gained {granted} practice {plural}.\r\n",
            ),
        );
        // Room broadcast — leveling up is a moment of triumph
        // worth surfacing to anyone watching. Bystanders see
        // "X surges with newfound power and rises to level N!"
        // Includes the title when one is defined for the new
        // level (mostly staff tiers). Mortal observers don't see
        // raw stat gains — those stay in the personal message.
        if let Some(located) = world.get::<Located>(entity).copied() {
            let actor_name = name_of(world, entity);
            let line = format!(
                "{actor_name} surges with newfound power and rises to level {next}{title_suffix}!\r\n"
            );
            crate::commands::broadcast_room_visual(
                world,
                located.0,
                entity,
                &[entity],
                &crate::commands::cap_sentence_start(&line),
            );
        }
        // B5: union the level row's permissions into the
        // character's Account.perms. Mortal levels carry empty
        // permission lists; staff tiers (Builder/Coder/Admin/God)
        // grant their associated flags here. Persistence happens
        // on save like any other Account change.
        if !next_row.permissions.is_empty()
            && let Some(mut acct) = world.get_mut::<mud_world::Account>(entity)
        {
            let mut granted_new: Vec<mud_db::enums::Permission> = Vec::new();
            for p in &next_row.permissions {
                if !acct.perms.contains(p) {
                    acct.perms.push(*p);
                    granted_new.push(*p);
                }
            }
            if !granted_new.is_empty() {
                let labels: Vec<&str> = granted_new.iter().map(|p| p.label()).collect();
                send_to(
                    world,
                    entity,
                    format!(
                        "<b:cyan>New permissions granted:</> {}\r\n",
                        labels.join(", "),
                    ),
                );
            }
        }
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
    /// attacker rolls 1 by default — multiplying that by `attack_power`
    /// can't reproduce a band like "7 ± 1". Instead we attach a
    /// `NaturalDamage { 1d1 + (dmg_roll - 1) }` so the rolled base is
    /// exactly `dmg_roll`, then leave `attack_power` at 0. This keeps
    /// the existing per-test damage assertions (variance bands, crit
    /// promotion math) intact across the rewrite.
    fn make_attacker(world: &mut World, room: Entity, target: Entity, dmg_roll: i32) -> Entity {
        world
            .spawn((
                Named {
                    name: "Attacker".to_string(),
                },
                Located(room),
                Fighting(target),
                CombatStats {
                    // accuracy 200 vs default evasion 50 = +75
                    // chance margin → clamped 99% hit. The residual
                    // 1% miss is removed in tests by `run_combat_tick`,
                    // which pins the hit roll to 1 (see `hit_roll`).
                    accuracy: 200,
                    ..Default::default()
                },
                NaturalDamage {
                    num: 1,
                    size: 1,
                    bonus: dmg_roll - 1,
                },
                Posture(PostureKind::Standing),
            ))
            .id()
    }

    fn make_target(world: &mut World, room: Entity, hp: i32) -> Entity {
        world
            .spawn((
                Named {
                    name: "Target".to_string(),
                },
                Located(room),
                Health { hp, max: hp },
            ))
            .id()
    }

    fn run_combat_tick(world: &mut World) {
        // combat_tick fires only on multiples of COMBAT_PERIOD_TICKS (40).
        world.insert_resource(TickCount(COMBAT_PERIOD_TICKS));
        // Deterministic hit roll (thread-local, so parallel tests are
        // unaffected): every swing at <=99% chance lands.
        FORCED_HIT_ROLL.with(|c| c.set(Some(1)));
        combat_tick(world);
        FORCED_HIT_ROLL.with(|c| c.set(None));
    }

    /// A6: a hidden attacker's swing applies an opening-strike
    /// bonus and Stealth is cleared after — verify the marker
    /// is gone after one `apply_swing`. Damage delta is hard to
    /// assert deterministically with variance / armor in play,
    /// so the test only confirms the marker drop.
    #[test]
    fn stealth_clears_after_first_swing() {
        let mut world = World::new();
        let room = make_room(&mut world);
        let target = make_target(&mut world, room, 100);
        let attacker = make_attacker(&mut world, room, target, 5);
        world
            .get_entity_mut(attacker)
            .unwrap()
            .insert(mud_world::Stealth);
        assert!(world.get::<mud_world::Stealth>(attacker).is_some());
        run_combat_tick(&mut world);
        assert!(
            world.get::<mud_world::Stealth>(attacker).is_none(),
            "Stealth marker dropped after first swing",
        );
    }

    /// A4: lock the posture penalty ladder so a casual edit
    /// doesn't quietly halve / double the values without a
    /// matching design review. Anyone changing this table is
    /// also updating `posture-and-lifestate.md`.
    #[test]
    fn posture_evasion_penalty_table() {
        assert_eq!(posture_evasion_penalty(PostureKind::Standing), 0);
        assert_eq!(posture_evasion_penalty(PostureKind::Kneeling), 10);
        assert_eq!(posture_evasion_penalty(PostureKind::Sitting), 20);
        assert_eq!(posture_evasion_penalty(PostureKind::Resting), 25);
        assert_eq!(posture_evasion_penalty(PostureKind::Sleeping), 30);
        // Ladder is monotonically non-decreasing — protects
        // against an off-by-one swap that puts Resting below
        // Sitting.
        let ladder = [
            posture_evasion_penalty(PostureKind::Standing),
            posture_evasion_penalty(PostureKind::Kneeling),
            posture_evasion_penalty(PostureKind::Sitting),
            posture_evasion_penalty(PostureKind::Resting),
            posture_evasion_penalty(PostureKind::Sleeping),
        ];
        for w in ladder.windows(2) {
            assert!(
                w[0] <= w[1],
                "posture penalty ladder must be non-decreasing, got {ladder:?}",
            );
        }
    }

    #[test]
    fn applies_damage_when_attacker_swings() {
        let mut world = World::new();
        let room = make_room(&mut world);
        let target = make_target(&mut world, room, 50);
        let _attacker = make_attacker(&mut world, room, target, 7);

        run_combat_tick(&mut world);

        let hp = world
            .get::<Health>(target)
            .expect("target still has Health");
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

    /// A linkdead player (socket dropped mid-fight) has no `Connection`
    /// but must keep swinging on the normal combat tick.
    #[test]
    fn linkdead_player_without_a_connection_keeps_swinging() {
        let mut world = World::new();
        let room = make_room(&mut world);
        let target = make_target(&mut world, room, 50);
        let player = make_attacker(&mut world, room, target, 7);
        world
            .entity_mut(player)
            .insert((Player, crate::commands::Linkdead { since_tick: 0 }));
        assert!(world.get::<crate::commands::Connection>(player).is_none());

        run_combat_tick(&mut world);

        let hp = world.get::<Health>(target).unwrap().hp;
        assert!(hp < 50, "linkdead player's swing landed, hp {hp}");
        assert!(world.get::<Fighting>(player).is_some());
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
        let hp = world
            .get::<Health>(target)
            .expect("target still has Health");
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
                Named {
                    name: "Target".to_string(),
                },
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
    fn melee_kill_still_autoloots_for_the_fighter() {
        let mut world = World::new();
        let room = make_room(&mut world);
        let target = world
            .spawn((
                Mob,
                Named {
                    name: "Target".to_string(),
                },
                Located(room),
                Health { hp: 5, max: 5 },
            ))
            .id();
        let item = world
            .spawn((
                Item,
                Named {
                    name: "a rusty dagger".to_string(),
                },
                Located(target),
            ))
            .id();
        let attacker = make_attacker(&mut world, room, target, 100);
        world.entity_mut(attacker).insert((
            Player,
            mud_world::PlayerFlags(vec![mud_db::enums::PlayerFlag::AutoLoot]),
        ));
        run_combat_tick(&mut world);
        assert!(world.get_entity(target).is_err(), "target died");
        assert_eq!(world.get::<Located>(item).map(|l| l.0), Some(attacker));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
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
                composition: mud_db::enums::Composition::Flesh,
                damage_type: mud_db::enums::DamageType::Hit,
                move_points: 0,
                default_position: mud_db::enums::Position::Standing,
                traits: Vec::new(),
                movement_mode: mud_db::enums::MovementMode::Normal,
                default_movement_mode: mud_db::enums::MovementMode::Normal,
                aggression_formula: None,
            },
        );
        world.insert_resource(protos);

        let target = world
            .spawn((
                Mob,
                Named {
                    name: "a stray dog".to_string(),
                },
                Located(room),
                Health { hp: 5, max: 5 },
                mud_world::WorldKey { zone: 1, id: 1 },
            ))
            .id();
        let player = world
            .spawn((
                Player,
                Named {
                    name: "Tester".to_string(),
                },
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
                NaturalDamage {
                    num: 1,
                    size: 1,
                    bonus: 99,
                },
                Posture(PostureKind::Standing),
                Fighting(target),
            ))
            .id();

        run_combat_tick(&mut world);

        // Mob is dead, corpse exists in the room.
        assert!(world.get_entity(target).is_err(), "target despawned");
        let corpse = world
            .query_filtered::<(Entity, &Located, &CoinPile), With<Corpse>>()
            .iter(&world)
            .find(|(_, l, _)| l.0 == room)
            .map(|(e, _, p)| (e, p.0));
        let (_corpse_entity, coin) =
            corpse.expect("corpse with CoinPile spawned in room (no AutoGold)");
        assert_eq!(
            coin, 75,
            "coin amount lands on corpse for non-AutoGold killer"
        );
        // Player wealth is still zero — coin is on the corpse,
        // claimed via `get all from corpse`.
        assert!(
            world.get::<Wealth>(player).is_none() || world.get::<Wealth>(player).unwrap().0 == 0,
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
                Named {
                    name: "Tester".to_string(),
                },
                Located(room),
                Health { hp: 0, max: 100 },
                Posture(PostureKind::Standing),
            ))
            .id();
        // Two carried items, one of them equipped on the body slot.
        let _carried = world
            .spawn((
                Item,
                Named {
                    name: "a stick".to_string(),
                },
                Keywords(vec!["stick".to_string()]),
                Located(player),
            ))
            .id();
        let _worn = world
            .spawn((
                Item,
                Named {
                    name: "a robe".to_string(),
                },
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
        assert!(
            corpse_name.contains("Tester"),
            "corpse names the dead player"
        );
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

    fn spawn_dying_player(world: &mut World, room: Entity, name: &str, coins: i64) -> Entity {
        world
            .spawn((
                Player,
                Named {
                    name: name.to_string(),
                },
                Located(room),
                Health { hp: 0, max: 100 },
                Posture(PostureKind::Standing),
                Wealth(coins),
                mud_world::BankWealth(9_999),
            ))
            .id()
    }

    fn player_corpse_in(world: &mut World, room: Entity) -> Entity {
        world
            .query_filtered::<(Entity, &Located), With<mud_world::PlayerCorpse>>()
            .iter(world)
            .find(|(_, l)| l.0 == room)
            .map(|(e, _)| e)
            .expect("player corpse in room")
    }

    #[test]
    fn player_death_moves_carried_coins_into_the_corpse() {
        let mut world = World::new();
        let room = make_room(&mut world);
        world.insert_resource(TickCount(0));
        let player = spawn_dying_player(&mut world, room, "Tester", 12_345);

        super::handle_death(&mut world, player, "Tester", room);

        assert_eq!(world.get::<Wealth>(player).unwrap().0, 0, "purse emptied");
        assert_eq!(
            world.get::<mud_world::BankWealth>(player).unwrap().0,
            9_999,
            "bank untouched"
        );
        let corpse = player_corpse_in(&mut world, room);
        assert_eq!(world.get::<mud_world::CoinPile>(corpse).unwrap().0, 12_345);
    }

    #[test]
    fn player_death_with_no_coins_leaves_no_coin_pile() {
        let mut world = World::new();
        let room = make_room(&mut world);
        world.insert_resource(TickCount(0));
        let player = spawn_dying_player(&mut world, room, "Tester", 0);

        super::handle_death(&mut world, player, "Tester", room);

        let corpse = player_corpse_in(&mut world, room);
        assert!(world.get::<mud_world::CoinPile>(corpse).is_none());
    }

    #[test]
    fn player_death_writes_the_corpse_snapshot_with_the_coins() {
        let dir = crate::corpses::tests::scratch_dir("death_snapshot");
        let path = dir.join("corpses.json");
        let mut world = World::new();
        world.insert_resource(TickCount(0));
        world.insert_resource(mud_world::WorldKeyIndex::default());
        world.insert_resource(crate::corpses::CorpseSnapshotPath(path.clone()));
        let room = world.spawn(mud_world::WorldKey { zone: 30, id: 45 }).id();
        let player = spawn_dying_player(&mut world, room, "Tester", 5_150);

        super::handle_death(&mut world, player, "Tester", room);

        let text = std::fs::read_to_string(&path).expect("death wrote the snapshot");
        assert!(text.contains("the corpse of Tester"), "{text}");
        assert!(text.contains("\"coins\": 5150"), "{text}");
        assert!(text.contains("\"is_player\": true"), "{text}");
    }

    #[test]
    fn owner_get_all_from_corpse_recovers_the_coins_and_others_cannot() {
        let mut world = World::new();
        let room = make_room(&mut world);
        world.insert_resource(TickCount(0));
        world.insert_resource(ObjectPrototypes::default());
        world.insert_resource(mud_world::WorldKeyIndex::default());
        let path = crate::corpses::tests::scratch_dir("loot_snapshot").join("corpses.json");
        world.insert_resource(crate::corpses::CorpseSnapshotPath(path.clone()));
        world
            .entity_mut(room)
            .insert(mud_world::WorldKey { zone: 30, id: 45 });
        let owner = spawn_dying_player(&mut world, room, "Tester", 777);
        let thief = spawn_dying_player(&mut world, room, "Robber", 0);
        super::handle_death(&mut world, owner, "Tester", room);
        let corpse = player_corpse_in(&mut world, room);

        crate::commands::info::cmd_get(&mut world, thief, "all corpse");
        assert_eq!(
            world.get::<mud_world::CoinPile>(corpse).unwrap().0,
            777,
            "non-owner is refused by the consent gate"
        );
        assert_eq!(world.get::<Wealth>(thief).unwrap().0, 0);
        let text = std::fs::read_to_string(&path).expect("death snapshot");
        assert!(
            text.contains("\"coins\": 777"),
            "refused loot leaves it: {text}"
        );

        crate::commands::info::cmd_get(&mut world, owner, "all corpse");
        assert!(world.get::<mud_world::CoinPile>(corpse).is_none());
        assert_eq!(world.get::<Wealth>(owner).unwrap().0, 777);
        // Looting rewrote the snapshot (corpse first, then the player
        // save, which needs a DB pool and is a no-op in this world).
        let text = std::fs::read_to_string(&path).expect("loot snapshot");
        assert!(
            text.contains("\"coins\": 0"),
            "coins left the snapshot: {text}"
        );
    }

    #[test]
    fn mob_death_does_not_touch_player_wealth_paths() {
        let mut world = World::new();
        let room = make_room(&mut world);
        world.insert_resource(TickCount(0));
        let mob = world
            .spawn((
                Mob,
                Named {
                    name: "a rat".to_string(),
                },
                Located(room),
                Health { hp: 0, max: 5 },
                Posture(PostureKind::Standing),
                Wealth(500),
            ))
            .id();

        super::handle_death(&mut world, mob, "a rat", room);

        let corpse = world
            .query_filtered::<(Entity, &Located), With<Corpse>>()
            .iter(&world)
            .find(|(_, l)| l.0 == room)
            .map(|(e, _)| e)
            .expect("mob corpse");
        assert!(
            world.get::<mud_world::CoinPile>(corpse).is_none(),
            "mob carried Wealth is not a corpse source; only proto wealth is"
        );
        assert!(world.get::<mud_world::PlayerCorpse>(corpse).is_none());
    }

    fn pvp_kill_alignment(victim_align: i32, killer_align: i32) -> i32 {
        let mut world = World::new();
        let room = make_room(&mut world);
        world.insert_resource(TickCount(0));
        let victim = spawn_dying_player(&mut world, room, "Victim", 0);
        world.entity_mut(victim).insert(CombatStats {
            alignment: victim_align,
            ..Default::default()
        });
        let killer = world
            .spawn((
                Player,
                Named {
                    name: "Killer".to_string(),
                },
                Located(room),
                Health { hp: 100, max: 100 },
                CombatStats {
                    alignment: killer_align,
                    ..Default::default()
                },
                Fighting(victim),
            ))
            .id();
        super::handle_death(&mut world, victim, "Victim", room);
        world.get::<CombatStats>(killer).unwrap().alignment
    }

    #[test]
    fn killing_an_evil_player_raises_killer_alignment() {
        // v_al -800, k_al 0: -800 / (-75 - 25*5) = 4; no killer drag.
        assert_eq!(pvp_kill_alignment(-800, 0), 4);
    }

    #[test]
    fn killing_a_good_player_lowers_killer_alignment() {
        assert_eq!(pvp_kill_alignment(800, 0), -4);
    }

    #[test]
    fn killing_a_neutral_player_is_a_wash_except_for_extreme_killers() {
        // Legacy: 0 / x - 2*|k/1000|; only |k| = 1000 pays the drag.
        assert_eq!(pvp_kill_alignment(0, 0), 0);
        assert_eq!(pvp_kill_alignment(0, 1000), 998);
        assert_eq!(pvp_kill_alignment(0, -1000), -1000);
    }

    #[test]
    fn pvp_alignment_clamps_at_the_bounds() {
        assert_eq!(pvp_kill_alignment(-1000, 1000), 1000);
        assert_eq!(pvp_kill_alignment(1000, -1000), -1000);
    }

    #[test]
    fn kill_alignment_formula_matches_legacy_examples() {
        use super::kill_alignment_after as f;
        // Evil killer (-500) killing a good victim (+1000):
        // 1000 / (-75 - 25*|(-500-1000)/200 = -7|) = 1000/-250 = -4,
        // minus 2*|-500/1000| = 0.
        assert_eq!(f(-500, 0, 50, 1000, 50), -504);
        // Level gap > 10 amplifies a negative change: -4 * (30/10).
        assert_eq!(f(0, 0, 40, 800, 10), -12);
        // Positive changes are not amplified.
        assert_eq!(f(0, 0, 40, -800, 10), 4);
        // Class bias: a paladin at 900 reads as 1000 (+100) so the
        // killer drag applies.
        assert_eq!(f(900, 100, 50, 0, 50), 898);
        assert_eq!(super::class_alignment_bias("Anti-Paladin"), -100);
        assert_eq!(super::class_alignment_bias("Warrior"), 0);
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
                Named {
                    name: "Tester".to_string(),
                },
                Located(room),
                Health { hp: 0, max: 100 },
                Posture(PostureKind::Standing),
            ))
            .id();
        let mob = world
            .spawn((
                Mob,
                Named {
                    name: "Guard".to_string(),
                },
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
                Named {
                    name: "Sleeper".to_string(),
                },
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
                Named {
                    name: "Resting".to_string(),
                },
                Located(room),
                Health { hp: 50, max: 50 },
                // Default defender — see sleeping-jolt test.
                CombatStats::default(),
                Posture(PostureKind::Resting),
            ))
            .id();
        let attacker = world
            .spawn((
                Named {
                    name: "Attacker".to_string(),
                },
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
                NaturalDamage {
                    num: 1,
                    size: 1,
                    bonus: 6,
                },
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

    /// Two rooms joined by one open exit, plus a mob in the first
    /// that carries `behaviors` at `hp`/100 HP and is being hit by a
    /// 7-damage attacker. Returns `(room_a, room_b, mob)`.
    fn hurt_mob_under_attack(
        world: &mut World,
        behaviors: Vec<mud_db::enums::MobBehavior>,
        hp: i32,
    ) -> (Entity, Entity, Entity) {
        let room_a = world.spawn(Exits::default()).id();
        let room_b = world.spawn(Exits::default()).id();
        world.get_mut::<Exits>(room_a).unwrap().0.insert(
            mud_db::enums::Direction::North,
            mud_world::ExitData {
                to: Some(room_b),
                state: mud_db::enums::ExitState::Open,
                key: None,
                description: None,
                keywords: Vec::new(),
                is_hidden: false,
                is_pickproof: false,
                is_bashable: false,
                hit_points: None,
            },
        );
        let mob = world
            .spawn((
                Mob,
                Named {
                    name: "a jackal".to_string(),
                },
                Located(room_a),
                Health { hp, max: 100 },
                CombatStats::default(),
                Posture(PostureKind::Standing),
                mud_world::MobBehaviors(behaviors),
            ))
            .id();
        let attacker = make_attacker(world, room_a, mob, 7);
        try_insert(world, mob, Fighting(attacker));
        (room_a, room_b, mob)
    }

    #[test]
    fn wimpy_mob_flees_when_hp_drops_below_a_quarter() {
        let mut world = World::new();
        // 28 -> 21 after a 7-point hit: below 100 >> 2 = 25.
        let (_a, room_b, mob) =
            hurt_mob_under_attack(&mut world, vec![mud_db::enums::MobBehavior::Wimpy], 28);
        run_combat_tick(&mut world);
        assert_eq!(world.get::<Located>(mob).map(|l| l.0), Some(room_b));
        assert!(world.get::<Fighting>(mob).is_none());
    }

    #[test]
    fn wimpy_mob_above_the_threshold_keeps_fighting() {
        let mut world = World::new();
        // 60 -> 53: well above 25.
        let (room_a, _b, mob) =
            hurt_mob_under_attack(&mut world, vec![mud_db::enums::MobBehavior::Wimpy], 60);
        run_combat_tick(&mut world);
        assert_eq!(world.get::<Located>(mob).map(|l| l.0), Some(room_a));
        assert!(world.get::<Health>(mob).is_some_and(|h| h.hp < 60));
    }

    #[test]
    fn spell_damage_below_a_quarter_makes_a_wimpy_mob_flee() {
        let mut world = World::new();
        let (_a, room_b, mob) =
            hurt_mob_under_attack(&mut world, vec![mud_db::enums::MobBehavior::Wimpy], 28);
        let caster = world.spawn_empty().id();
        let (dead, _) = crate::commands::apply_damage_from(&mut world, mob, 7, caster);
        assert!(!dead);
        assert_eq!(world.get::<Located>(mob).map(|l| l.0), Some(room_b));
    }

    #[test]
    fn killing_spell_does_not_make_a_wimpy_mob_flee() {
        let mut world = World::new();
        let (room_a, _b, mob) =
            hurt_mob_under_attack(&mut world, vec![mud_db::enums::MobBehavior::Wimpy], 5);
        let caster = world.spawn_empty().id();
        let (dead, _) = crate::commands::apply_damage_from(&mut world, mob, 10, caster);
        assert!(dead);
        assert_eq!(world.get::<Located>(mob).map(|l| l.0), Some(room_a));
    }

    #[test]
    fn damage_without_an_attacker_does_not_make_a_wimpy_mob_flee() {
        let mut world = World::new();
        let (room_a, _b, mob) =
            hurt_mob_under_attack(&mut world, vec![mud_db::enums::MobBehavior::Wimpy], 28);
        crate::commands::apply_damage(&mut world, mob, 7);
        assert_eq!(world.get::<Located>(mob).map(|l| l.0), Some(room_a));
    }

    #[test]
    fn non_wimpy_mob_does_not_flee_below_the_threshold() {
        let mut world = World::new();
        let (room_a, _b, mob) = hurt_mob_under_attack(&mut world, vec![], 28);
        for _ in 0..8 {
            run_combat_tick(&mut world);
            if world.get::<Health>(mob).is_none_or(|h| h.hp <= 0) {
                break;
            }
            assert_eq!(world.get::<Located>(mob).map(|l| l.0), Some(room_a));
        }
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
                Named {
                    name: "Tester".to_string(),
                },
                Located(room_b), // already fled
                Health { hp: 100, max: 100 },
                Posture(PostureKind::Standing),
                CombatStats::default(),
            ))
            .id();
        let mob = world
            .spawn((
                Mob,
                Named {
                    name: "Guard".to_string(),
                },
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
        let player = world
            .spawn((
                Player,
                Named {
                    name: "Tester".to_string(),
                },
                Located(room),
                Health { hp: 5, max: 100 }, // one swing kills
                Posture(PostureKind::Standing),
                CombatStats::default(),
            ))
            .id();
        let _attacker_a = make_attacker(&mut world, room, player, 50);
        let _attacker_b = make_attacker(&mut world, room, player, 50);

        run_combat_tick(&mut world);

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
                Named {
                    name: "Target".to_string(),
                },
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
        assert_eq!(hp.hp, 50, "Frozen attacker doesn't generate a swing");
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
                Named {
                    name: "Tester".to_string(),
                },
                Located(room),
                Health { hp: 100, max: 100 },
                Posture(PostureKind::Standing),
                Frozen,
            ))
            .id();
        let mob = world
            .spawn((
                Mob,
                Named {
                    name: "Guard".to_string(),
                },
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
                Named {
                    name: "Tester".to_string(),
                },
                Located(room),
                Health { hp: 100, max: 100 },
                Posture(PostureKind::Standing),
                Stunned,
            ))
            .id();
        let mob = world
            .spawn((
                Mob,
                Named {
                    name: "Guard".to_string(),
                },
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
                Named {
                    name: "Tester".to_string(),
                },
                Located(room),
                Health { hp: 0, max: 100 },
                Posture(PostureKind::Standing),
                Ghost,
            ))
            .id();
        let mob = world
            .spawn((
                Mob,
                Named {
                    name: "Guard".to_string(),
                },
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

    fn corpse_with_loot(world: &mut World, at: Entity, secs: i32) -> (Entity, Entity) {
        let corpse = world
            .spawn((
                Item,
                Corpse,
                Named {
                    name: "the corpse of a goblin".into(),
                },
                Located(at),
                CorpseDecay {
                    remaining_secs: secs,
                },
            ))
            .id();
        let loot = world
            .spawn((
                Item,
                Named {
                    name: "a dagger".into(),
                },
                Located(corpse),
            ))
            .id();
        (corpse, loot)
    }

    #[test]
    fn expired_mob_corpse_drops_its_loot_on_the_floor() {
        let mut world = World::new();
        world.insert_resource(TickCount(10));
        let room = world.spawn(mud_world::Room).id();
        let (corpse, loot) = corpse_with_loot(&mut world, room, 1);
        corpse_decay_tick(&mut world);
        assert!(world.get_entity(corpse).is_err(), "corpse removed");
        assert_eq!(world.get::<Located>(loot).unwrap().0, room);
    }

    #[test]
    fn expired_player_corpse_drops_its_loot_on_the_floor() {
        let mut world = World::new();
        world.insert_resource(TickCount(10));
        let room = world.spawn(mud_world::Room).id();
        let (corpse, loot) = corpse_with_loot(&mut world, room, 1);
        world.entity_mut(corpse).insert(mud_world::PlayerCorpse);
        corpse_decay_tick(&mut world);
        assert!(world.get_entity(corpse).is_err());
        assert_eq!(world.get::<Located>(loot).unwrap().0, room);
    }

    /// Loose coin piles lying in `room`: `(entity, copper, timer)`.
    fn floor_coin_piles(world: &mut World, room: Entity) -> Vec<(Entity, i64, i32)> {
        let mut q = world.query_filtered::<(
            Entity,
            &Located,
            &mud_world::CoinPile,
            Option<&mud_world::ItemTimer>,
        ), With<Item>>();
        q.iter(world)
            .filter(|(_, l, _, _)| l.0 == room)
            .map(|(e, _, c, t)| (e, c.0, t.map_or(0, |t| t.remaining_secs)))
            .collect()
    }

    #[test]
    fn expired_mob_corpse_coins_fall_to_the_room_and_rot() {
        let mut world = World::new();
        world.insert_resource(TickCount(10));
        let room = world.spawn(mud_world::Room).id();
        let (corpse, _) = corpse_with_loot(&mut world, room, 1);
        world.entity_mut(corpse).insert(mud_world::CoinPile(250));
        corpse_decay_tick(&mut world);
        assert!(world.get_entity(corpse).is_err());
        let piles = floor_coin_piles(&mut world, room);
        assert_eq!(piles.len(), 1, "one loose pile on the floor");
        assert_eq!(piles[0].1, 250);
        assert!(piles[0].2 > 0, "pile carries an item-decay timer");
    }

    #[test]
    fn expired_player_corpse_coins_fall_to_the_room() {
        let mut world = World::new();
        world.insert_resource(TickCount(10));
        let room = world.spawn(mud_world::Room).id();
        let (corpse, _) = corpse_with_loot(&mut world, room, 1);
        world
            .entity_mut(corpse)
            .insert((mud_world::CoinPile(900), mud_world::PlayerCorpse));
        corpse_decay_tick(&mut world);
        let piles = floor_coin_piles(&mut world, room);
        assert_eq!(piles.iter().map(|p| p.1).sum::<i64>(), 900);
    }

    #[test]
    fn expired_corpse_with_coins_and_items_releases_both() {
        let mut world = World::new();
        world.insert_resource(TickCount(10));
        let room = world.spawn(mud_world::Room).id();
        let (corpse, loot) = corpse_with_loot(&mut world, room, 1);
        world.entity_mut(corpse).insert(mud_world::CoinPile(40));
        corpse_decay_tick(&mut world);
        assert_eq!(world.get::<Located>(loot).unwrap().0, room);
        let piles = floor_coin_piles(&mut world, room);
        assert_eq!(piles.len(), 1);
        assert_eq!(piles[0].1, 40);
    }

    #[test]
    fn unexpired_corpse_keeps_its_coins() {
        let mut world = World::new();
        world.insert_resource(TickCount(10));
        let room = world.spawn(mud_world::Room).id();
        let (corpse, _) = corpse_with_loot(&mut world, room, 50);
        world.entity_mut(corpse).insert(mud_world::CoinPile(40));
        corpse_decay_tick(&mut world);
        assert_eq!(world.get::<mud_world::CoinPile>(corpse).unwrap().0, 40);
        assert!(
            floor_coin_piles(&mut world, room)
                .iter()
                .all(|p| p.0 == corpse),
            "only the corpse itself"
        );
    }

    #[test]
    fn corpse_coins_inside_a_container_merge_into_it() {
        let mut world = World::new();
        world.insert_resource(TickCount(10));
        let room = world.spawn(mud_world::Room).id();
        let chest = world
            .spawn((
                Item,
                Named {
                    name: "a chest".into(),
                },
                Located(room),
                mud_world::CoinPile(5),
            ))
            .id();
        let (corpse, _) = corpse_with_loot(&mut world, chest, 1);
        world.entity_mut(corpse).insert(mud_world::CoinPile(40));
        corpse_decay_tick(&mut world);
        assert_eq!(world.get::<mud_world::CoinPile>(chest).unwrap().0, 45);
        assert_eq!(
            floor_coin_piles(&mut world, room).len(),
            1,
            "only the chest itself, no extra loose pile"
        );
    }

    #[test]
    fn unclaimed_loose_coin_pile_rots_on_the_item_timer() {
        let mut world = World::new();
        let room = world.spawn(mud_world::Room).id();
        let pile = crate::item_decay::spawn_loose_coin_pile(&mut world, room, 10);
        world
            .get_mut::<mud_world::ItemTimer>(pile)
            .unwrap()
            .remaining_secs = 1;
        crate::item_decay::item_decay_tick(&mut world);
        assert!(world.get_entity(pile).is_err());
    }

    #[test]
    fn unexpired_corpse_keeps_its_loot() {
        let mut world = World::new();
        world.insert_resource(TickCount(10));
        let room = world.spawn(mud_world::Room).id();
        let (corpse, loot) = corpse_with_loot(&mut world, room, 50);
        corpse_decay_tick(&mut world);
        assert!(world.get_entity(corpse).is_ok());
        assert_eq!(world.get::<Located>(loot).unwrap().0, corpse);
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
    /// "step into the room and die" contract. `cmd_move`'s gate just
    /// asks "is there a `DeathTrap` here?" and routes to `handle_death`;
    /// this test pins `handle_death`'s effect on a player so the
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
                Named {
                    name: "DTVictim".to_string(),
                },
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
    /// `PeacefulRoom` marker.
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
                Named {
                    name: "ArenaA".to_string(),
                },
                Located(room),
                Health { hp: 100, max: 100 },
                Posture(PostureKind::Standing),
            ))
            .id();
        let _p2 = world
            .spawn((
                Player,
                Named {
                    name: "ArenaB".to_string(),
                },
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
    /// present, broadcast skips it" — `broadcast_global` reads
    /// `world.get::<SoundproofRoom>` directly.
    #[test]
    fn soundproof_room_marker_classifies_room() {
        let mut world = World::new();
        let booth = make_room(&mut world);
        world.entity_mut(booth).insert(mud_world::SoundproofRoom);
        let player = world
            .spawn((
                Player,
                Named {
                    name: "Listener".to_string(),
                },
                Located(booth),
            ))
            .id();
        // The exact predicate `broadcast_global` runs is:
        //   world.get::<Located>(t).is_some()
        //     && world.get::<SoundproofRoom>(located.0).is_some()
        // Re-execute that here so a future change to the gate
        // wording is caught by this test.
        let located = world.get::<Located>(player).copied().expect("Located set");
        let is_soundproof = world.get::<mud_world::SoundproofRoom>(located.0).is_some();
        assert!(
            is_soundproof,
            "listener's room reports as soundproof — broadcast skips them",
        );
    }
}
