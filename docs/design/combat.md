# Combat

**Status:** proposal — awaiting review.

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
   attacker_score = attacker.accuracy  + d100
   defender_score = defender.evasion   + d100
   if attacker_score <= defender_score: MISS, stop.

2. Crit roll
   crit = d100 <= attacker.crit_chance
   crit_mult = 1.5 if crit else 1.0

3. Base damage
   base = weapon.base_damage * (1 + attacker.attack_power / 100)
   base *= crit_mult
   base *= 1 + uniform(-VARIANCE, +VARIANCE)        # default ±25%

4. Penetration vs armor
   effective_armor_pct = max(0, defender.armor_pct - attacker.pen_pct)
   base *= 1 - effective_armor_pct / 100
   base -= max(0, defender.armor_flat - attacker.pen_flat)

5. Wards (magical layer)
   base *= 1 - defender.ward_pct / 100

6. Type resistance
   resist = defender.resistances[weapon.damage_type] or 0    # signed -100..+100
   base *= 1 - resist / 100

7. Hardness floor
   final = max(0, base - defender.hardness)
```

Magical attacks (`Ability.uses_stat == MAGICAL`) substitute `spell_power`
for `attack_power` and use the ability's own `base_damage` formula
instead of `weapon.base_damage`. Everything else is identical.

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

A mob without a wielded item still attacks. Two options:

- **(A)** Mobs always wield an implicit "natural attack" weapon: a
  virtual `Weapon { base_damage: <mob.attack_power*0.4>, damage_type:
  Bludgeoning }`. No per-mob weapon row needed.
- **(B)** `Mobs.natural_base_damage Int @default(5)` and
  `Mobs.natural_damage_type DamageType @default(BLUDGEONING)` columns,
  with `MobResetEquipment` overriding when the mob spawns wielding
  something. Builders can author "the dragon's bite is FIRE base 30."

Recommendation: (B). Two columns, one default row per mob, makes
fanged/clawed/breath-attacker mobs author cleanly without a separate
"natural weapon" object table.

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

## Open questions

1. **Variance band.** ±25% (sketched), ±10% (predictable), or ±50%
   (lottery)?
2. **Crit multiplier.** 1.5× (proposal), 2× (classic), or stat-driven
   (`crit_damage` column too)?
3. **Negative resistance cap.** Vulnerability lets `resist` go below 0,
   amplifying damage. Cap at −100% (2× damage), or unbounded?
4. **Mob natural weapon shape.** (A) implicit virtual weapon vs (B) two
   columns on `Mobs`. I recommended B.
5. **`ward_pct` source.** A flat column on the entity, or always from
   active `EffectInstance`s with a "ward" tag and the runtime sums
   them? Stat column is simpler; effect-tag is more dynamic. I'd
   default to **a stat column that gets *modified* by ward effects**
   via the existing `modify` effect-type — same pattern as
   strength buffs.
6. **PvP scaling.** Should accuracy/evasion be scaled when both sides
   are players? Today equal-stat players hit each other 50% which is
   probably fine. Note for review.

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
