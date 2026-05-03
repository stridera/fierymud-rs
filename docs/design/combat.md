# Combat

**Status:** locked except where noted (review pass 1, 2026-05-03).

## Design intent

Replace the legacy AC / THAC0 / dice triumvirate with a modern
**comparison-based** model: every swing resolves through three independent
comparisons (hit, mitigation, type-resist) and a single damage roll.

Goals:

- Stat sheets read clearly: bigger number means better (accuracy, attack
  power, evasion, armor). No "lower AC is better" trap.
- Damage formulas live in code as a fixed pipeline; per-row content
  authoring is just numbers in columns. No formula language for combat.
- Showdice rendering is trivial: every line carries the same five values
  whether the toggle is on or off.
- Scales linearly. A level-50 mob having `accuracy = 200` is meaningful
  next to a level-50 player with `evasion = 180` — the gap is the
  story, not the absolute number.

## Pipeline

Every swing runs this exact sequence. No skipped phases, no alternate
formulas per weapon:

```
1. Hit roll
   attacker_score = attacker.accuracy + d100        # d100 ∈ [1, 100]
   defender_score = defender.evasion  + d100
   if attacker_score <= defender_score: MISS, stop.   # ties go to attacker

2. Crit roll
   crit_mult = 2.0 if d100 <= attacker.crit_chance else 1.0

3. Base damage
   base = weapon.base_damage * (1 + attacker.attack_power / 100)
   base *= crit_mult
   base *= 1 + uniform(-0.25, +0.25)                 # ±25% variance band

4. Penetration vs armor
   effective_armor_pct = max(0, defender.armor_pct - attacker.pen_pct)
   base *= 1 - effective_armor_pct / 100
   base -= max(0, defender.armor_flat - attacker.pen_flat)

5. Wards (magical layer)
   base *= 1 - defender.ward_pct / 100

6. Type resistance
   resist = defender.resistances[weapon.damage_type] or 0
   # range: capped at +100 (immunity); negative is UNBOUNDED (vulnerability)
   base *= 1 - resist / 100

7. Hardness floor
   final = max(0, base - defender.hardness)
```

Magical attacks (`Ability.uses_stat == MAGICAL`) substitute `spell_power`
for `attack_power` and use the ability's own `base_damage` formula
instead of `weapon.base_damage`. Everything else is identical.

### Stat reference

**Accuracy / Evasion**

- Baseline 50 produces a 50% hit rate against an equal-stat opponent.
  A 5-point stat advantage shifts hit rate by ~5%.
- No hard maximum. Level-50 trained warriors might run 200; boss-tier
  defenders 250. Content authors are trusted not to push absurdly.
- Negative is fine. A blinded attacker with `accuracy = -20` still
  lands occasionally when their d100 rolls high. Posture penalties,
  debuff effects, and cursed gear all push these into the negative.
- Tie semantics: ties go to the attacker, so equal stats produce
  exactly 50% (not 49.5%).

**Crit chance / multiplier**

- `crit_chance` is 0–100. Default 5.
- Crit multiplier is a flat **2×**. No `crit_damage` stat for v1; add
  the column later if a class needs "rogue crits do 3×."

**Variance band**

- Uniform multiplier on damage: ±25% means a 100-base hit lands
  somewhere in [75, 125]. Compounds with crit: critical hits land
  in [150, 250] for the same 100-base swing.
- Per-target. In a room AOE the variance rolls fresh for each target
  (so two enemies don't take exactly the same damage from one fireball).

**armor_flat vs hardness — the same shape, different layer**

Both subtract a flat amount, but at different points:

- `armor_flat` sits at step 4. Counterable by `pen_flat`. This is
  what player gear contributes to.
- `hardness` sits at step 7, after type resist and after armor. **Not
  counterable.** This is "this dragon has adamantine scales" — boss
  content authors use this to guarantee a fight stays meaningful no
  matter how much pen the party stacks.

Most mobs ship with `hardness = 0`. It's a content lever for big
encounters, not a default.

**Vulnerability is unbounded**

A boss with `resistances: { "LIGHTNING": -500 }` takes 6× lightning
damage. This is the design intent: content authors can express
"raid puzzle: bring lightning damage." Immunity caps at +100; there's
no symmetric vulnerability cap.

## Schema

Columns required on `Mobs` and `Characters` (drop the legacy ones):

| Column | Type | Default | Notes |
|---|---|---|---|
| `accuracy` | Int | 50 | Hit-roll vs evasion |
| `evasion` | Int | 50 | Defender side of hit-roll |
| `attack_power` | Int | 0 | Physical damage multiplier (% additive) |
| `spell_power` | Int | 0 | Magical damage multiplier (% additive) |
| `crit_chance` | Int | 5 | 0–100 percent |
| `pen_pct` | Int | 0 | Reduces effective `armor_pct` (0–100) |
| `pen_flat` | Int | 0 | Reduces effective `armor_flat` |
| `armor_pct` | Int | 0 | Percent damage mitigation (0–100) |
| `armor_flat` | Int | 0 | Flat soak per swing |
| `ward_pct` | Int | 0 | Magical mitigation (0–100) |
| `hardness` | Int | 0 | Floor — damage below this is zeroed |
| `resistances` | Json | `{}` | `{ "FIRE": 25, "MENTAL": -25 }` |

`Objects` (weapons): see [objects.md](objects.md). Adds `base_damage`
and `damage_type`. Drops `Hit Dice` JSON.

**Drop from `Mobs` and `Characters`:**
- `armor_class` (replaced by `armor_pct` + `armor_flat`)
- `hit_roll` (replaced by `accuracy`)
- `damage_roll` (replaced by `attack_power`)
- `hp_dice_num` / `hp_dice_size` / `hp_dice_bonus` (HP comes from
  `level` × class growth + `Health.max` directly; no boot-time roll)
- `damage_dice_*` on `Mobs` (replaced by `attack_power` + an implicit
  weapon — see "Mobs and weapons" below)

## Runtime

`combat::tick_one_pair` reads the columns above directly from
`CombatStats` (extended) plus `EquippedSlot::Wield` for the weapon. The
swing-snapshot pre-pass already collects per-attacker weapon dice today
(commit 1bcac19); replace it with weapon `base_damage` + `damage_type`.

The `showdice` toggle (`PlayerFlag::ShowDiceRolls`) extends every swing
line with the resolved values:

```
You hit a goblin for 47 damage.
  (acc 85 vs ev 60 — pen 20% vs 30% armor — fire vs 25% resist — 8 hardness)
```

Without the toggle, the line is just the first row.

## Mobs and weapons

A mob without a wielded item still attacks. **Locked: option (B)** —
two columns on `Mobs`:

- `natural_base_damage Int @default(5)`
- `natural_damage_type DamageType @default(BLUDGEONING)`

`MobResetEquipment` overrides at spawn time when the mob actually
wields something. Builders can author "the dragon's bite is FIRE
base 30" cleanly; no virtual-weapon table.

### Mob armor and loot are decoupled

A clean separation that's worth highlighting because it lets content
authors describe armored mobs without forcing a full equipment drop:

- **Mob defensive numbers** (`armor_pct`, `armor_flat`, `ward_pct`,
  `hardness`, `resistances`) are intrinsic columns on `Mobs`. They
  do **not** come from equipped items.
- **Visual armor** is just text in `Mobs.description` /
  `Mobs.room_description`. The runtime never reads it for combat.
- **Loot** is a separate system (`MobResetEquipment` rows that drop
  on death, plus future `MobLoot` tables for non-equipment drops).
  Most mobs drop little or nothing.

So a content author can write:

```
description:       a hulking ogre clad in iron plates
armor_pct:         40
armor_flat:        8
loot:              100 copper, a bone fragment
```

The player sees armor, fights armor, doesn't get a free suit of
plate from every kill. Intentional.

For *humanoid* mobs that should drop their gear (bandits, guards),
`MobResetEquipment` is the existing mechanism: the mob spawns
wielding/wearing the item, the item's stats stack additively on top
of the mob's intrinsic armor (clothes don't make the man — they
augment), and the item drops on death. Opt-in per mob.

## Healing and overkill

- Healing applies straight to `Health.hp`, capped at `max`. No
  comparison pipeline.
- Lethal damage zeroes `Health.hp` exactly; no negative-HP "dying"
  state. Death handler runs at hp=0.
- Over-damage is discarded. (If we want corpse-explosion mechanics
  later, that's a separate effect.)

## Examples

**Two evenly-matched warriors:** acc 100 vs ev 100, weapon 20, no
armor, no resist. Hit gate is 50/50. On hit:
`20 * 1.0 * crit * 1±0.25` → 15–25 typical, 23–37 on crit.

**Heavy plate vs light dagger:** acc 90 vs ev 60, weapon 8 piercing,
defender armor_pct 60 / armor_flat 5 / pen 0.
Effective armor_pct = 60. `8 * 1.0 * (1±0.25) → 6–10`. After armor:
`6–10 * 0.4 = 2.4–4.0`. After flat: `0`. Plate works.

**Armor-piercing crit:** acc 90 vs ev 60, weapon 8 piercing, attacker
pen_pct 40, defender armor_pct 60.
Effective armor_pct = 20. `8 * 1.5 * (1±0.25) ≈ 9–15`, after 20% armor
`7.2–12`, no flat soak loss. Crit got through.

**Fire wand vs water elemental:** acc 80 vs ev 50, ability base 30
fire, defender fire_resist −50 (vulnerable).
`30 * (1+0.5) = 45 base`, no armor, no ward.

## Decisions locked (review pass 1, 2026-05-03)

| Question | Locked |
|---|---|
| Variance band | **±25%** (uniform multiplier; per-target in AOE) |
| Crit multiplier | **2×** (flat; no `crit_damage` stat for v1) |
| Negative resistance cap | **Unbounded vulnerability**; +100 immunity cap stays |
| Mob natural weapon | **(B)** two columns on `Mobs` — `natural_base_damage`, `natural_damage_type` |
| `ward_pct` source | **Stat column** on entity, modified by ward-tagged `modify` effects |
| PvP scaling | **No scaling** — equal players hit at 50%, which is fine |
| Tie-break on hit roll | Ties go to **attacker** (so equal stats = exactly 50% hit) |
| Accuracy/Evasion baseline | **50/50 = 50% hit rate**; no hard cap; negatives allowed |
| Hardness vs armor_flat | Both flat subtract; `pen_flat` counters `armor_flat` only — `hardness` is **unbypassable** |

## Remaining open questions

None blocking the migration. The following are tuning knobs we can
revisit after the first content pass:

- Whether to add a soft cap on accuracy/evasion (e.g. 300) once
  content scales reveal whether numbers run away.
- Whether crit needs a `crit_damage` column for class differentiation
  (rogues 3× crits, etc.) — easy to add later.
- Whether `armor_flat` should be readable from the equipped armor
  item proto and the mob's intrinsic value summed at swing time, or
  pre-summed onto the entity at equip time. Implementation detail.

## Migration plan

Once approved, the Prisma migration:

1. Add the new columns on `Mobs`, `Characters`, `Objects` with sensible
   defaults.
2. fierylib seeders / re-import populates from legacy values:
   `accuracy = 50 + hit_roll * 2`, `evasion = 50 + dex_bonus * 5`,
   `attack_power = damage_roll * 5`, `armor_pct = clamp(0, 80,
   (10 - armor_class) * 5)`. (These mappings are tunable — a balance
   pass after re-import.)
3. Runtime switches to reading the new columns; legacy columns become
   dead.
4. Schema cleanup: drop the dead columns in a follow-up migration.
