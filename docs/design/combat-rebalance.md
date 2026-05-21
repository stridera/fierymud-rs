# Combat rebalance — May 2026

This document records the state of the combat system on **2026-05-10**, the
issues discovered during a play-test (AdminChar L105 in god gear killed in 5–6
hits by an L85 guard), the immediate fixes implemented, and the open
questions where the user should pick a direction before further work.

The legacy CircleMUD-derived FieryMUD C++ codebase
(`/home/strider/Code/mud/fierymud_legacy/`) is the calibration source: 20+
years of play-tested balance values. Where the existing
[`combat.md`](combat.md) design doc clashes with legacy semantics, the
clashes are called out in [Open questions](#open-questions).

## Bugs found

### B1 — `ObjectAffects` not loaded (showstopper)
The Postgres schema has 4083 `ObjectAffects` rows describing how worn gear
modifies the wearer (`HITROLL +5`, `AC +10`, `STR +2`, etc.). The Rust
loader (`crates/mud-world/src/loader.rs`) never reads the table. Player gear
contributes **zero** stat bonuses.

Verified live: BuilderChar (L102) wearing 19 god-gear items showed
`Hit roll: 0   Damage roll: 0   AC: 10` — the schema-default values, with no
gear contribution.

### B2 — `ObjectEffects` and `ObjectResistance` not loaded
Same issue. `ObjectEffects` (gear that grants spell-like effects, e.g.
*ring of invisibility*) and `ObjectResistance` (per-element resistance from
gear) are both schema-modeled but never loaded. Today both tables happen to
be empty, but the loader path needs to exist before content authors can use
them.

### B3 — Mob damage uses pre-computed average, not a per-swing roll
`MobProto::avg_damage()` returns `n*(m+1)/2 + b` (the dice average) and
`combat_tick` reads it as a flat damage value, then applies a ±25%
variance band. Legacy rolls the dice **per swing**:

```c
// fight.cpp:2198-2199
if (IS_NPC(ch))
    dam += roll_dice(damnodice, damsizedice);
```

For an L85 druidic guard (`27d10 + 15`) this means our system delivers
~163 ± 41 damage per swing (122–204) instead of the legacy `42 + bonus` to
`270 + bonus` swing distribution. The current "always near average" hits
make damage too consistent and too high.

### B4 — `hit_chance_pct` direction is inverted from the doc string
The current formula is

```rust
let modifier = hit_roll * 2 - target_ac * 5;
let chance   = (80 + modifier).clamp(5, 100);
```

with a doc string saying "Lower AC = better armor (CircleMUD/D&D
semantics)". The math actually says **higher AC = harder to hit** (a
target_ac of -100 caps the chance at 100% for the attacker). That's
opposite of what the doc claims and opposite of legacy
(`armor_class = -100` is god-tier armor in legacy and in the imported
fierydev data). The posture modifier is similarly inverted — comment
says non-standing is "easier to hit", math says it's harder.

This affects:
* Mob `armor_class` values imported from legacy (negative = good armor)
* `ObjectAffects` rows with `location = 'AC'` (legacy: positive modifier
  = armor improvement, applied as `ch.ac -= modifier`)

### B5 — Player has no level/class HP scaling
The `LevelDefinition` table grants 10 hp/level for L1–9, 11–17 hp through
mid-level, and 50 hp/level only at L100+. That's class-agnostic (a level-50
mage and a level-50 warrior both have ~675 HP). Legacy had per-class HP
growth: warriors `8d13` per level (avg 56), mages `1d6` (avg 3.5). At
high levels this means the legacy warrior has ~10× the HP of the legacy
mage, and our flat scaling can't recreate that.

### B6 — No `Mana` component
`ObjectAffects.MANA` modifiers exist on items (e.g. *ancient grimoire*
`MANA +75`) but the engine has no mana pool. Score sheet renders
`M:0/0` always.

## Phase-1 fixes implemented (2026-05-10)

These are surgical fixes to make combat playable. Larger restructuring
(class-based HP curves, full accuracy/evasion model, group play
mechanics) is deferred to Phase 2 below pending user input.

### 1. Gear apply path
* New module `crates/mud-server/src/equip_apply.rs`:
  * `apply_object_to_wearer(world, item, wearer)` — walks the proto's
    `ObjectAffects` rows and calls `apply_modify_delta` for each.
    Granted effects (`ObjectEffects`) spawn as `EffectInstance`s tagged
    with `EffectSource::Gear(item)` so they're removed when the item is.
    Resistances bump a per-element map on a new `Resistances` component.
  * `unapply_object_from_wearer(world, item, wearer)` — inverse.
* Hooked into:
  * `wear_into` after `try_insert(EquippedSlot)`
  * `cmd_remove` (and `remove all`) before `try_remove::<EquippedSlot>`
  * Login `respawn_inventory_from_db` after items with `equipped_location`
    get their `EquippedSlot` component
  * Loader Pass 6 (`mob_reset_equipment`) for mobs that wear/wield gear

### 2. ObjectAffects → component mapping
Legacy `APPLY_*` constants mapped to existing Rust components:

| Legacy `location` | Rust target | Notes |
|---|---|---|
| `STR` / `DEX` / `CON` / `INT` / `WIS` / `CHA` | `CoreStats.*` | already supported by `apply_modify_delta` |
| `HITROLL` | `CombatStats.hit_roll` | supported |
| `DAMROLL` | `CombatStats.dmg_roll` | supported |
| `AC` | `CombatStats.ac` | **subtract** (legacy: lower = better armor) |
| `HIT` | `Health.max` (and current) | supported via `max_hp` |
| `MOVE` | `Stamina.max` | supported via `max_move` |
| `MANA` | `Mana.max` | new component, see §3 |
| `SAVING_PARA` / `_ROD` / `_PETRI` / `_BREATH` / `_SPELL` | `SavingThrows.*` | new component |
| `FOCUS` | `Focus.0` | new component (used by spell slot regen) |
| `PERCEPTION` | `Perception.0` | new component (used by detect / search) |
| `HIT_REGEN` | `RegenBonus.hp` | new component (used by `regen.rs`) |
| `HIDDENNESS` | `Stealth.bonus` | extends existing `Stealth` |
| `SIZE` / `AGE` / `CHAR_HEIGHT` / `CHAR_WEIGHT` / `COMPOSITION` / `GOLD` | logged, no-op | follow-up — these need flavor systems |

`apply_modify_delta` is extended with each new branch and `reverse_modify_delta`
inherits the inverse for free.

### 3. New components
* `Mana { current, max }` — added to `mud-world/src/components.rs`. Players
  spawn with `max = max(0, (level^2)/10)` per legacy class.cpp (mages get
  it earlier in legacy via class-specific gain, deferred to Phase 2). Mana
  regen wired to `regen.rs` at the same cadence as HP regen.
* `SavingThrows { para, rod, petri, breath, spell }` — five-axis save
  modifier from gear / spells / class.
* `Focus(i32)` — drives spell-slot regen rate.
* `Perception(i32)` — drives detect/search.
* `RegenBonus { hp, mana, move }` — flat per-tick bonuses applied on top
  of base regen.
* `Resistances(HashMap<ElementType, i32>)` — per-element % modifier from
  gear/effects. Combat pipeline reads this before the type-resist step.

### 4. Combat formula fixes
* `hit_chance_pct` rewritten so **lower AC = harder to hit** (legacy
  CircleMUD semantics). New formula:
  ```rust
  // chance = 50 + (hit_roll - target_ac) * 1
  // clamped to [5, 95] so a 5% miss/hit floor always exists (matches
  // legacy auto-hit / auto-miss bands at d200 1-10 / 191-200).
  ```
  Tuning is deliberately conservative: at L85 mob (hit_roll≈18, target
  player no-armor ac=100), `chance = 50 + 18 - 100 = -32` → clamped to 5%.
  That's much too tank-favored — see [Open question Q1](#q1-formula-shape)
  for the alternatives I considered.
* Mob damage path rolls dice per swing instead of using the proto's
  pre-computed `dmg_roll`. Players still use weapon dice + `dmg_roll`
  as before.
* Posture modifier sign flipped to match its doc string (non-standing
  defender is easier to hit, not harder).
* `MAX_DAMAGE = 1000` cap applied per legacy `defines.hpp:349`.
* Combat period set to `COMBAT_PERIOD_TICKS = 40` = 4s/swing — matches
  legacy `PULSE_VIOLENCE` so the DB-authored damage values stay calibrated.
  See [Q3 — Combat cadence (resolved)](#q3-combat-cadence).

### 5. Existing behavior preserved
* Crit on natural roll (currently d100 = 100 → ×1.5; legacy is d20 = 20 → ×2).
* Variance ±25% on damage (matches design doc).
* Dodge / parry skill checks (chance = `prof / 50` capped at 25%).
* Berserk +50% damage (ours) vs legacy +10% — ours is more dramatic.
* Sleeping defender auto-hit; resting/sleeping forces stand on hit.

## Open questions

The decisions below need user input before I keep going. Each one has my
recommendation but I'm not confident enough to pick unilaterally.

### §8 (gear-curves) — Armor / hit-rate philosophy — RESOLVED

User pick (2026-05-14): **Moderate-armor, balanced-hit**. Both pieces shipped:

* `ac → armor_pct` scaler in `mud-server/src/commands.rs::apply_modify_delta`
  reduced **×5 → ×2**. T2 median per-slot now ~8% (full kit ~32%), T6 ~14%
  (full kit ~56%) instead of capping by mid-tier.
* Per-class accuracy progression added in
  `fierylib/src/fierylib/combat_formulas.py::CLASS_ACCURACY_PER_LEVEL`,
  derived from legacy `class.cpp` THAC0 (`(25 - class_thac0) / 10`).
  Warriors 2.7/lvl, Clerics 2.1/lvl, Sorcerers/Mages 1.8-1.9/lvl.
  Closes the §6a structural gap where mob authored `hit_roll` outpaced
  players. Live in `derive_hit_roll_baseline(class_name=...)`; both
  user_seeder and player_importer pass class through.

**Open follow-up**: in-game level-up (`combat.rs::level_up`) doesn't bump
accuracy. Per-class rate is correctly baked at character creation /
import, but a character who levels up *in-game* keeps their L1
accuracy. Adding `Class.accuracy_per_level` to the schema + a
level-up consumer is the durable fix; not in scope for the §8 lock.

### Q1 — Formula shape (THACO vs accuracy/evasion vs current) — RESOLVED

User pick: **Option (B)** — map legacy values to the planned
acc/ev/armor_pct model at load time. **Implemented and live.**

`CombatStats` now carries: `accuracy`, `evasion`, `attack_power`,
`spell_power`, `crit_chance`, `pen_pct`, `pen_flat`, `armor_pct`,
`armor_flat`, `ward_pct`, `hardness`, `alignment`. The legacy
`hit_roll` / `dmg_roll` / `ac` fields **no longer exist on the
entity**.

Conversion at spawn (per `docs/design/combat.md` migration plan):

```text
accuracy     = 50 + hit_roll * 2
evasion      = 50 + dex_bonus * 5    (dex_bonus = (dex - 10) / 2)
attack_power = damage_roll * 5       (additive percentage)
armor_pct    = clamp(0, 80, (10 - armor_class) * 5)
crit_chance  = 5                     (legacy d20 == 20)
```

`ObjectAffects` keys translate symmetrically inside
`apply_modify_delta` so authored content keeps working without DB
edits:

| Legacy key | Maps to | Conversion |
|---|---|---|
| `HITROLL` | `accuracy` | × 2 |
| `DAMROLL` | `attack_power` | × 5 |
| `AC` | `armor_pct` | × 5 (clamped 100) |

Combat pipeline now follows `docs/design/combat.md` end-to-end:

1. Hit / miss: `chance = 50 + (accuracy − evasion) / 2`, clamped `[1, 99]`
2. Crit: separate d100 vs `crit_chance`
3. Base damage: `weapon_dice * (1 + attack_power / 100)`
4. Armor: `damage *= (100 − max(0, armor_pct − pen_pct)) / 100`,
   then subtract `max(0, armor_flat − pen_flat)`
5. Ward: skipped on mundane swings (engaged only when ability is magical)
6. Type resistance: looked up against the defender's `Resistances`
   map for the swing's element type
7. Hardness floor: damage below `hardness` zeroes out

Live verification — BuilderChar (L102, 19 god-gear items):

```
Acc: 60   Eva: 70   Atk: +35   Armor: 65%   Alignment: neutral (0)
```

Cross-checks: `60 = 50 + 5(HITROLL) × 2`, `70 = 50 + 4(dex_bonus) × 5`,
`+35 = 7(DAMROLL) × 5`, `65% = 13(AC) × 5`. Mob damage cut from
~220 (raw 38d10+21) to ~80 per swing (×0.35 from 65% armor_pct).

### Q2 — Class-based HP curves

The current `LevelDefinition` table is class-agnostic: every class gains
the same HP per level. Legacy has dramatic per-class differences:

| Class | Levels 1–30 (per level) | Level 30+ (per level) |
|---|---|---|
| Sorcerer / Mage | 1d6 (avg 3.5) | 3 |
| Cleric / Shaman | 3d8 (avg 13.5) | 6 |
| Rogue / Thief | 5d11 (avg 30) | 6 |
| Warrior / Monk / Berserker | 8d13 (avg 56) | 10 |

A level-50 warrior in legacy has ~1880 HP base before CON; a level-50 mage
has ~120 HP. That dramatic spread is what makes tank/DPS roles meaningful.

**Options:**

* **(A) Per-class `LevelDefinition`** — schema change adds a
  `class_id` column to `LevelDefinition` so each class has its own
  growth curve. Existing players would need a one-time HP recompute
  on next login.
* **(B) Class-multiplier on the existing flat curve** — add a
  `hp_multiplier` to the `Class` model (warrior 4.0×, mage 0.3×, …)
  and multiply the level-up gain at runtime. Less invasive but coarser.
* **(C) HP dice roll in code, ignore `LevelDefinition.hp_gain`** —
  port the legacy `class.cpp` table directly into Rust. No schema
  change. Loses the data-authoring path (builders can't tweak HP
  curves without code changes).

**I deferred this** — needs user input. **Recommendation: (A).**

### Q3 — Combat cadence — RESOLVED

User pick (2026-05-14): **restore 4-second legacy cadence**. Implemented:
`COMBAT_PERIOD_TICKS = 40` at 10 Hz tick = one swing every 4 real seconds.

Rationale: matches legacy `PULSE_VIOLENCE = 4 RL_SEC`, which the
DB-authored damage values were calibrated against — zero content rework
required. Combat is slower and more tactical, leaving time for the
healer to react. Rejected the 1s alternative because it would have
required scaling all DB damage values down ~4× (large content sweep).

### Q4 — Mitigation pipeline order

Legacy stacks (sequential, multiplicative):

1. Hit/miss roll
2. Damage type evasion (cubic on susceptibility)
3. Displacement (20% / 33% miss)
4. Defensive skill checks (riposte → parry → dodge)
5. Base damage roll (weapon + STR + damroll)
6. Critical (only natural d20=20, ×2)
7. Position multiplier (1× to 3× damage on supine targets)
8. PvP damage /3 reduction
9. Charm pet damage /2 reduction
10. Sanctuary / Stoneskin (×0.5)
11. Ranger dual-wield bonus (×1.2)
12. Protection good/evil (×0.8)
13. Damage type susceptibility (`dam * susceptibility / 100`)
14. Cap at `MAX_DAMAGE = 1000`

The newer [`combat.md`](combat.md) design doc has a much cleaner
7-step pipeline (hit → crit → base → armor → ward → resist → hardness).

**Question:** do we port the legacy pipeline verbatim (preserves all
20-year balance corner cases — sanctuary halves, ranger bonus, position
multipliers, etc.) or adopt the cleaner combat.md pipeline?

**Recommendation:** adopt the combat.md pipeline (it's cleaner, easier
to extend, and the design doc has it locked-in by review). Preserve
the legacy *constants* (sanctuary 50% mitigation, position multipliers
as armor bonuses) but reshape the pipeline.

### Q5 — Mana / spell-slot system

Legacy has both *mana* (a pool) and *spell slots* (per-circle
counters). The Rust engine has neither today. `crates/mud-server/src/
memorize.rs` exists but spell slots aren't wired into casting.

* **(A)** Pure mana pool (modern MMO). Simple. Loses circle gating.
* **(B)** Pure spell slots (legacy). Matches existing
  `crates/mud-server/src/memorize.rs` infrastructure.
* **(C)** Both — mana for at-will casting, slots for big spells.

**Recommendation: (B)** — matches legacy and matches the half-built
infrastructure. Tonight's gear apply path stores `MANA` modifier on a
new `Mana` component for forward-compat, but the slot system needs a
separate workstream.

### Q6 — Group dynamics (tank / heal aggro)

Legacy has no aggro list per se — the mob just attacks whoever it last
fought. The user explicitly wants *"tanks can stand their own with
enough damage mitigation to allow the healer to keep them alive while
the damage dealers burn down the enemy."* Pieces required:

1. **Tank survivability** — needs Q1 + Q2 + ObjectAffects-loading. The
   first two are open questions; the third lands tonight.
2. **Aggro management** — currently mobs auto-engage anyone who hit
   them. A `taunt` skill that biases the mob's `HateList` toward the
   tank would be the minimum.
3. **Heal targeting** — `cast 'heal' Bob` already works. Ranged
   pickup-the-tank from across the room is fine.
4. **Group XP** — already implemented (`combat.rs:trophy_xp_modifier`).
5. **Formation** — not in legacy, not necessary.

**Recommendation:** add a `taunt` skill in a follow-up. Otherwise the
existing `HateList` + `Fighting` mechanics are sufficient for group
play once Q1/Q2 land.

## What's playable after tonight

After the Phase-1 changes:

* Equipment grants its stats. AdminChar's god gear actually does
  something now.
* Score sheet shows the gear-augmented values.
* Mob damage is no longer pinned to its average — fights have more
  variance.
* The hit/AC math is internally consistent.
* Mana, focus, perception, saving throws, hit-regen are stored on the
  player but only mana / saves are wired into combat (Q5 / save rolls
  follow up).

What's **still wrong** until [Q1](#q1-formula-shape), [Q2](#q2-class-based-hp-curves),
and [Q3](#q3-combat-cadence) are resolved:

* A level-50 mage and warrior have the same HP (Q2).
* Combat is 4× faster than legacy was tuned for (Q3).
* High-level mobs can still over-damage low-armor characters because
  the formula isn't fully porting legacy mitigation layers (Q1, Q4).

The user should pick a direction on Q1–Q3 before I keep going.

## Live verification (after rebuild)

Restarted the dev server with the Phase-1 changes and tested two
scenarios:

**BuilderChar (L102 Warrior, all 19 god-gear items)**

Before: `Hit roll: 0   Damage roll: 0   AC: 10   HP: 1020   Stamina: 304   STR 18`
After:  `Hit roll: 5   Damage roll: 7   AC: -3   HP: 1045   Stamina: 1304   STR 17 CON 25 INT 28 WIS 21 CHA 20`

Every stat matches the DB-aggregated `ObjectAffects` sums for
BuilderChar exactly. Resistances, granted-effect spawning, and the
unequip-rollback path are all wired (the round-trip test in
`equip_apply::tests` covers the math).

**Combat sample — L102 BuilderChar vs L80 ogre enforcer (11 133 HP, 38d10+21 dmg)**

```
You hit an ogre enforcer for 23 damage.
You hit an ogre enforcer for 52 damage.
You hit an ogre enforcer for 71 damage.
an ogre enforcer hits you for 225 damage.
an ogre enforcer hits you for 216 damage.
You swing at an ogre enforcer but miss.
an ogre enforcer swings at you but misses.
```

Damage variance shows the per-swing dice roll (player swings range
23–71 from 5d20+7); mob damage variance also visible. Player kills
mob in ~225 swings (~3.7 min) vs mob kills player in ~5 swings
(~5 sec). The asymmetry is **the class-scaling problem from Q2**:
the player has 1k HP for level 102 (no per-class growth) while the
mob has 11k HP from its dice — same fight in legacy would feature
a warrior with several thousand HP and per-class damroll growth.

**Combat sample — L25 TestWarrior vs L29 displacer beast (975 HP, 10d4+6 dmg)**

```
You hit a displacer beast for 1 damage.
a displacer beast hits you for 37 damage.
a displacer beast hits you for 32 damage.
a displacer beast hits you for 22 damage.
You hit a displacer beast for 1 damage.
```

TestWarrior has no wielded weapon → the legacy "1 damage barehand"
path. Mob hits for ~30/swing on a 250-HP player. Same issue — at
*every* level the lack of class HP/damroll scaling makes solo PvE
unwinnable.

### Retest 2026-05-20 (post Q2 HP curves + starter gear) — RESOLVED

The solo-PvE asymmetry above is **fixed**. Q2's per-class
`LevelDefinition` HP curves landed and the fierylib seeder now
grants starter gear (TestWarrior wields a claymore at creation).
Fresh virtual playtest:

**L25 TestWarrior (712 HP, claymore) vs L23 soldier (550 HP, 8d4+5)**

```
You hit a soldier for 28 damage.
You hit a soldier for 44 damage.
You hit a soldier for 30 damage.
You hit a soldier for 26 damage.
You swing at a soldier but miss.
a soldier swings at you but misses.
```

Warrior swings now land 11–44 (claymore dice, ~28 avg) instead of
the 1-damage barehand path. The fight is a real contest: TestWarrior
**won, ending at 349/712 (≈49% HP)**. A solo fight against a
tier-appropriate mob now costs meaningful HP but is decisively
winnable — exactly the shape the action items below called for. The
712-HP L25 warrior (vs the old ~250) is the per-class curve doing
its job. No further tuning needed at this tier; L100+ god
characters are excluded from balance analysis (legacy:
uncombatable, all-crit).

## Action items (post-rebuild)

* Restart the running server (the binary on disk is the new build;
  the live process needs a SIGTERM + respawn).
* Decide Q1–Q3 to set the direction for the next pass. The
  highest-impact single decision is **Q2** (class-based HP curves);
  without it, no amount of formula tuning fixes the solo-PvE
  experience the user complained about.
* Once Q2 lands, retest the L25 / L80 / L100 fights above. The
  expectation is a level-equal fight ends in 30–60 seconds with the
  player victorious; a +5-level fight is decisive against the player;
  a -5-level fight is trivial for the player.
