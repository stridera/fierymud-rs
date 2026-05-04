# Damage Types & Resistances

**Status:** locked except where noted (review pass 1, 2026-05-03).

## Design intent

A single `DamageType` enum used by every damage source — weapon
swings, ability rows, environmental hazards, breath weapons, traps.
Resistances on entities live in one JSON column keyed by the same
enum. No duplicate vocabulary between mob resistances, weapon types,
and ability damage tags.

## Enum

```
DamageType:
  # Physical
  SLASHING
  PIERCING
  BLUDGEONING

  # Elemental
  FIRE
  COLD
  LIGHTNING
  ACID
  SONIC

  # Mystic
  HOLY
  NECROTIC
  MENTAL
  POISON

  # Universal
  TRUE         # bypasses all resistance and armor; reserved for
               # scripted unblockable hits
```

12 named types plus `TRUE` as the explicit "no mitigation" value.

## Resistance application

Defender's `resistances` JSON is `{"FIRE": 25, "MENTAL": -25}`.
Positive numbers reduce damage; negative numbers amplify
(vulnerability). Immunity caps at `+100`; **vulnerability is
unbounded** — a boss row of `{"LIGHTNING": -500}` deals 6× lightning
damage on purpose, so content authors can encode "raid puzzle: bring
lightning damage." Aligned with [combat.md](combat.md)'s locked
decision.

Applied **after** armor and ward (see [combat.md](combat.md) pipeline
step 6) so type resistance is the last reduction before hardness.

## TRUE damage

`TRUE`-typed damage skips all of armor, ward, type resist, and
hardness. Used sparingly — boss-only telegraphed hits, narrative
"dragon swallows you" effects. The runtime treats it as a full bypass
of pipeline steps 4–7 in combat.md.

## Mitigation engagement — two independent axes

The combat pipeline has two pre-resist mitigation steps and they
key on different things:

| Step | Layer | Engaged when |
|---|---|---|
| 4 (`armor_pct`/`armor_flat`) | physical | Damage type's **category** is PHYSICAL |
| 5 (`ward_pct`) | magical | Damage source's **`Ability.is_magical`** is true |

Type resist (step 6) and hardness (step 7) apply to all non-`TRUE`
damage regardless.

The two axes intentionally don't co-vary. A mundane torch's fire
is ELEMENTAL **and** non-magical → skips both layers, only fire-resist
mitigates. A dragon's breath is ELEMENTAL **and** magical → skips
armor, engages ward. A wizard's enchanted-steel blade does SLASHING
(physical, engages armor) on the base swing and FIRE (elemental,
skips armor) + magical (engages ward) on the on-hit.

### Damage categorization

Damage types group into four categories. The **armor** axis above
reads from category alone; nothing else does.

```
DamageCategory:
  PHYSICAL  -> SLASHING, PIERCING, BLUDGEONING
  ELEMENTAL -> FIRE, COLD, LIGHTNING, ACID, SONIC
  MYSTIC    -> HOLY, NECROTIC, MENTAL, POISON
  UNIVERSAL -> TRUE
```

Chainmail soaks a sword swing but not a fireball because of this
table — no per-callsite branch.

## Mixed-damage weapons

Every weapon has exactly one base `damage_type`. A "flaming sword"
that does *slashing + fire* is composed: the base swing is
**slashing** (PHYSICAL — armor applies), and the fire portion is an
**on-hit ability** the weapon also carries (ELEMENTAL — armor skipped
automatically). This keeps weapon authoring clean (one type column)
and makes the fire portion reusable — the same `flame_strike`
ability can sit on a flame brand, on a torch, on a fire elemental's
natural attack, and on a `cast firestrike` spell.

A torch is the canonical case: a stick (BLUDGEONING base) lit on one
end (on-hit FIRE). Hitting someone with a torch lands the bludgeoning
through their armor and the fire around it.

### How a flaming sword resolves

Weapon: 12 base slashing + on-hit `flame_strike` (8 base fire).
Defender (plain goblin): no armor, no ward, no resists, no hardness.
Variance is rolled mid-band for clarity.

**Pass 1 — slashing swing** runs the full combat.md pipeline. SLASHING
is PHYSICAL, so steps 4-7 all apply: hit roll, crit, base × variance,
armor (none here), ward, slashing resist, hardness. The slashing
portion lands like any normal swing → 12 damage.

**Pass 2 — on-hit fire ability** fires only if the swing landed.
The `flame_strike` ability is `is_magical = true`, so ward (step 5)
applies. FIRE is ELEMENTAL, so armor (step 4) is skipped. Type
resist (fire-resist, step 6) and hardness (step 7) apply.

→ 8 damage on the plain goblin (no ward, no fire-resist, no hardness
to bite).

A *mundane* on-hit (e.g. lit-torch `flame_burn` with `is_magical =
false`) skips ward in addition to armor — only fire-resist and
hardness mitigate it. Different ability row, same FIRE damage type.

The two damage numbers are emitted as separate lines so the player
can see both contributions:

```
You slash a goblin for 12 damage.
The flame brand burns the goblin for 8 damage.
```

### Vulnerability and immunity behave per-portion

Same flaming sword (12 slashing + 8 fire base). Different defenders,
otherwise unmitigated:

| Defender | Slashing portion | Fire portion | Total |
|---|---|---|---|
| Plain goblin (no resist) | 12 | 8 | **20** |
| Fire-immune (`{FIRE: 100}`) | 12 | **0** | **12** |
| Fire-vulnerable (`{FIRE: -50}`) | 12 | **12** | **24** |
| Fire-vulnerable (`{FIRE: -200}`) | 12 | **24** | **36** |

The slashing portion is unaffected by fire resist. The fire portion
sees only fire resist. This is the load-bearing property of the
on-hit composition — one weapon, two independent type-resist
checks.

### "Lit on fire" is a separate effect

The DoT ("burning") that ignites the target *after* the hit is **not**
the fire damage above. It's a separate `EffectInstance` with `kind =
burning` (or whatever the Effect catalog row is) spawned by the
`flame_strike` ability via its `AbilityEffect` row. Its tick damage
flows through the same fire-resist (so a fire-immune target won't
ignite at all — chance × 0 ≈ 0).

Whether a weapon ignites is **opt-in per ability row**: a
plain-fire-damage `flame_strike` spawns no DoT; an "ignite" variant
adds an AbilityEffect row that spawns the `burning` instance with
some duration. Authors compose what they want.

### Schema wiring

The on-hit binding lives on the existing `ObjectAbilities` junction
table. Today it carries `(object, ability_id, level, charges)` for
scrolls / wands; add a `trigger ObjectAbilityTrigger` enum column
that distinguishes:

```
ObjectAbilityTrigger:
  USE        # `recite scroll` / `quaff potion` — manual invocation
  ON_HIT     # fires when the wielding actor lands a melee swing
  ON_WEAR    # fires when equipped (ring of fire resistance)
  ON_REMOVE  # fires when unequipped (cleanup of wear-state)
```

Equip-time caching: when an item with an `ON_HIT` row gets wielded,
the runtime stamps an `OnHitAbility(ability_id)` component on the
wielder so the per-swing path is a component lookup, not a DB
query.

## Conversion: abilities → damage types

`Ability.damage_type` column or per-AbilityEffect override. An ability
that does mixed damage (lightning + thunder) is two effect rows on the
ability, each with its own type. Already supported by
`AbilityDamageComponent` (the multi-element split table).
`AbilityDamageComponent.element` should reference `DamageType`.

## Mob resistance authoring

Builders fill a JSON in Muditor:

```json
{
  "FIRE": 50,
  "COLD": -25,
  "MENTAL": 100
}
```

Unspecified types default to 0. Storage stays JSON because the key
set is sparse — most mobs have resistances on 0–3 types.

A class-level resistance (Race / CharacterClass / item-derived)
modifies the same final resolved value via the existing `modify`
effect-type. The pipeline reads the **runtime-resolved** resistances —
which is the JSON sum of: mob's own + active modify effects.

## Schema

### `DamageType` enum
Promote the existing string-based vocabulary in
`MobResistance` / `Class.resistances` / `Object.values["Damage Type"]`
to a typed enum.

### `Mobs.resistances Json` — keep as JSON
Variable-keyed map; suitable for JSON.

### `Class.resistances Json` — keep as JSON
Same shape; same reason.

### `ObjectResistance` — already a junction table
Stays. Element column should reference `DamageType` enum.

### `Objects.damage_type` — new typed column
Replaces `Object.values["Damage Type"]` string. See
[objects.md](objects.md).

### Drop legacy
- `Effect.tags` keeps `"magic"` / `"buff"` / etc. — those aren't
  damage types and stay free-string.
- Any standalone `DamageElement` enum that exists elsewhere folds
  into `DamageType`. Single source of truth.

## Examples

### Fire mage vs water elemental

- weapon: staff, base 4 bludgeoning
- ability: `fireball`, base 30 fire
- defender resistances: `{ "FIRE": -50, "COLD": 75 }`
- attacker pen 0, defender armor 10/0, ward 0, hardness 0

Swing damage:
```
4 * (1 + 0) * (1 ± 0.25)             # 3 - 5
* (1 - 10/100)                       # 2.7 - 4.5
* (1 - 0)                            # ward
* (1 - 0/100)                        # bludgeoning resist 0
                                     # 2.7 - 4.5
```

Ability damage:
```
30 * (1 + spell_power)               # 30 base
* (1 ± 0.25)                         # 22.5 - 37.5
* (1 - 0)                            # ward
* (1 - (-50)/100) = * 1.5            # vulnerability
                                     # 33.75 - 56.25
```

### Holy paladin vs undead

- weapon: longsword base 12 slashing, on-hit ability `smite`
  base 8 holy
- defender resistances: `{ "NECROTIC": 75, "HOLY": -50 }`,
  `armor_pct = 30`, `ward_pct = 0`, `hardness = 0`
- attacker `pen_pct = 0`

Pass 1 — slashing swing through full pipeline:
```
12 * 1.0 * (1 ± 0.25)                # 9 - 15
* (1 - 30/100)                       # 6.3 - 10.5  (armor reduces)
* (1 - 0)                            # ward
* (1 - 0/100)                        # slashing resist 0
                                     # 6.3 - 10.5
```

Pass 2 — on-hit `smite` (skips physical armor):
```
8 * (1 + spell_power)                # 8 base
* (1 ± 0.25)                         # 6 - 10
                                     # (skip armor — fire/holy isn't soaked by chainmail)
* (1 - 0)                            # ward
* (1 - (-50)/100) = * 1.5            # holy vulnerability
                                     # 9 - 15
```

Total per swing: ~15 - 25 damage to the undead.

## Decisions locked (review pass 1, 2026-05-03)

| Question | Locked |
|---|---|
| Type count | **Ship 12** named types + `TRUE`. Delete unused after a content pass — easier than retroactively splitting. |
| Vulnerability cap | **Unbounded vulnerability**; `+100` immunity cap stays. Aligned with combat.md. |
| Physical-immune shorthand | **No.** Three keys (slash/pierce/bludgeon = 100) is fine, and explicit. |
| `TRUE` damage exposure | **Schema-locked.** No content path can author `damageType: TRUE` — runtime-emit-only via a separate `InternalDamageType` path. Prevents accidental unblockable spells. |
| Aliases (`KINETIC` / `FROST` / etc.) | **Importer-only.** fierylib maps legacy synonyms to the canonical enum on import. Runtime never sees aliases. |
| On-hit composition | **Model A** (one `damage_type` on the weapon + optional on-hit ability). Reuses `ObjectAbilities` junction with a `trigger ON_HIT` discriminator. |
| Fire-on-hit pipeline scope | **Skip physical armor**, apply ward + type resist + hardness. Magical damage from on-hit isn't soaked by chainmail. |

## Remaining open questions

1. **`uses_stat`-style branch on the weapon's on-hit ability.** Combat.md
   has a `uses_stat == MAGICAL` branch that swaps `attack_power` for
   `spell_power`. The on-hit fire portion should travel that same
   branch automatically (since it's an ability with its own
   `uses_stat`). Verify the combat pipeline routes the on-hit
   correctly without special-casing it as "the weapon's on-hit".
2. **Damage line emission order.** The example renders slashing then
   fire on separate lines. Crit on the swing — does the on-hit also
   crit? Two reads: (a) the swing crit applies to both portions
   (one roll, both portions multiply); (b) on-hit rolls its own
   crit independently. Recommendation: **(b)**, since the on-hit is
   a separate ability with its own crit-relevant stats. But this
   needs a one-liner in combat.md.
3. **Alias documentation home.** The aliases-locked decision puts
   the mapping inside fierylib (`KINETIC → BLUDGEONING`, `FROST →
   COLD`, etc). Where does the canonical list live so reviewers
   can audit it — a comment block on the importer fn, or a
   short reference doc next to fierylib's CLAUDE.md?
