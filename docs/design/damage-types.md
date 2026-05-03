# Damage Types & Resistances

**Status:** proposal — awaiting review.

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
(vulnerability). Range cap: `[-100, +100]`. -100 = double damage;
+100 = full immunity. Out-of-range values clamp.

Applied **after** armor and ward (see [combat.md](combat.md) pipeline
step 6) so type resistance is the last reduction before hardness.

## TRUE damage

`TRUE`-typed damage skips all of armor, ward, type resist, and
hardness. Used sparingly — boss-only telegraphed hits, narrative
"dragon swallows you" effects. The runtime treats it as a full bypass
of pipeline steps 4–7 in combat.md.

## Conversion: weapons → damage types

Every weapon has exactly one `damage_type`. A flaming sword that does
"slashing + fire" is two abilities under the hood — the swing is
`slashing` and an `on_hit` effect adds `fire`. Cleaner than tagging
the weapon with multiple types.

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

- weapon: longsword base 12 slashing, on-hit ability "smite"
  base 8 holy
- defender resistances: `{ "NECROTIC": 75, "HOLY": -50 }`

Slashing swing lands normally; smite does 8 → 12 (holy
vulnerability).

## Open questions

1. **Granularity.** Is 12 types right? Easy to add but hard to remove.
   Temptation list to merge: SONIC into BLUDGEONING (sonic damage
   isn't really a thing in classic MUD content)? PIERCING and
   SLASHING separate or one "sharp"? Recommendation: ship the 12,
   delete unused after a content pass.
2. **Vulnerability cap.** `-100` (2× damage) — hard cap. Or unbounded
   so a `-200` content row deals 3× damage? I prefer the cap; lets
   builders push numbers without breaking the math.
3. **Cross-type immunity.** Currently `+100` on a single type =
   immune. Some mobs want full physical immunity = `+100` on
   slashing/piercing/bludgeoning all three. JSON authoring handles
   that. Should we have a `physical_immune Bool` shorthand? Probably
   not — three keys is fine, and explicit.
4. **`TRUE` damage exposure to content.** Should `TRUE` be authorable
   in Muditor or reserved for code-emit-only? I'd lock it down at the
   schema level (no `damageType: TRUE` for player-castable abilities)
   to prevent content authors from accidentally writing unblockable
   spells.
5. **Damage type aliases.** Some legacy content uses synonyms (KINETIC
   for BLUDGEONING, FROST for COLD). The fierylib re-importer should
   map them. List of accepted aliases worth documenting?
