# Objects

**Status:** proposal — awaiting review.

## Design intent

`Object.values JSONB` is a CircleMUD residue. Every `ObjectType` has a
fixed-shape blob the loader has to string-parse back into typed
fields:

- `WEAPON` → `{"Hit Dice": {"num": "N", "size": "M", "bonus": B}, "Damage Type": "..."}`
- `LIGHT` → `{"Capacity": N, "Remaining": M}`
- `DRINKCONTAINER` / `FOUNTAIN` → `{"Liquid": "...", "Capacity": N, "Remaining": M, "Poisoned": bool}`
- `PORTAL` → `{"Destination": <legacy vnum>}`
- `BOARD` → `{"Pages": <legacy vnum doubling as Board.id>}`
- `CONTAINER` → `{"Capacity": N}`

`loader.rs` lines 1438+ are five `parse_*` functions doing exactly
this. None of these shapes vary per-row within a type. They want to
be columns.

Worse: the `Destination` and `Pages` fields carry **CircleMUD vnums**
(`zone × 100 + id`) that the rest of the schema has banned. The
runtime decodes them via a `legacy_vnums` map. Promoting these to
proper composite-FK columns kills the last vnum hack outside trigger
content.

## Proposal

Drop `Object.values` JSONB entirely. Replace with typed columns,
nullable when not relevant for the type. The schema migration sets
each column based on the existing JSON; after that, content authors
edit columns directly in Muditor.

### `Objects` columns to add

| Column | Type | When set | Notes |
|---|---|---|---|
| `base_damage` | Int? | WEAPON | replaces Hit Dice (see [combat.md](combat.md)) |
| `damage_type` | DamageType? | WEAPON | enum, see [damage-types.md](damage-types.md) |
| `weapon_class` | WeaponClass? | WEAPON | sword, axe, dagger, bow, staff, … |
| `light_capacity` | Int? | LIGHT | game-hours; `-1` means infinite |
| `light_remaining` | Int? | LIGHT | per-spawn override allowed |
| `liquid_id` | Int? | DRINKCONTAINER, FOUNTAIN | FK → `Liquids.id` (typed, vs string today) |
| `liquid_capacity` | Int? | DRINKCONTAINER, FOUNTAIN | units |
| `liquid_remaining` | Int? | DRINKCONTAINER | fountains use `null` (= bottomless) |
| `liquid_poisoned` | Bool? | DRINKCONTAINER, FOUNTAIN | poison flag at proto time |
| `portal_dest_zone` | Int? | PORTAL | composite FK with portal_dest_room |
| `portal_dest_room` | Int? | PORTAL | proper FK to `Rooms` |
| `board_id` | Int? | BOARD | already FK-able; drop the "Pages doubles as Board.id" hack |
| `container_capacity` | Int? | CONTAINER | weight units |
| `container_lock_key_zone` | Int? | CONTAINER | composite FK to a Key item proto |
| `container_lock_key_id` | Int? | CONTAINER | |
| `food_hours` | Int? | FOOD | hunger restored when eaten |
| `armor_pct` | Int? | ARMOR | replaces hardcoded armor table; see combat.md |
| `armor_flat` | Int? | ARMOR | flat soak per swing |
| `pen_pct` | Int? | WEAPON | armor-pierce % |
| `pen_flat` | Int? | WEAPON | armor-pierce flat |

### Drop

- `Object.values` JSONB column.
- The legacy "Pages doubles as Board.id" and "Destination is a vnum"
  conventions.

### Move (re-name only — semantics already exist)

- `Objects.cost` (already a column) — no change.
- `Objects.weight` — no change.
- `Objects.level` — no change.
- `Objects.wear_flags` — no change.

## `Liquid` table

`Liquids` already exists. Convert `Object.values["Liquid"]` from a
string match to a real FK (`Objects.liquid_id`). The runtime already
loads `LiquidIndex` keyed by name; switch to keyed by id. Drink-effect
rows on `Liquids` (the `drunk_effect`) are already there.

## Runtime impact

Five `parse_*` functions in `loader.rs` get deleted:

- `parse_weapon_dice`
- `parse_liquid`
- `parse_light_fuel`
- `parse_portal_destination`
- `parse_board_id`

The `ObjectProto` struct shrinks: `weapon_dice_num/size/bonus` →
`base_damage` + `damage_type`; `liquid: Option<LiquidProto>` →
`liquid_id: Option<i32>` (with a separate `LiquidIndex.by_id` lookup
when needed); etc.

`LiquidContainer` ECS component switches its `liquid: String` to
`liquid_id: i32` (and the display name comes from a `LiquidIndex`
lookup at render time — same pattern as ability/effect catalogs).

Portals lose their `legacy_vnums` lookup. The loader can resolve
`portal_dest_zone` + `portal_dest_room` directly via `WorldKeyIndex`.

## Migration plan

1. Add the new columns on `Objects`. No defaults — every row gets
   its values back-filled from the existing JSON.
2. fierylib re-importer (or one-shot Python migration script) parses
   `Object.values` per row and writes the corresponding columns.
3. Add `Objects.liquid_id` as nullable; back-fill from
   `Liquids.find_by_name(values["Liquid"])`.
4. Runtime switches every read from JSON to the new columns.
5. Drop `Object.values` in a follow-up migration once nothing reads
   it.
6. Drop the `legacy_vnums` map in mud-world once Portals are
   migrated.

## Examples

A bronze longsword today:

```json
{ "Hit Dice": {"num": "1", "size": "12", "bonus": 0},
  "Damage Type": "slashing" }
```

Becomes:

```
base_damage = 12
damage_type = SLASHING
weapon_class = SWORD
pen_pct = 0
pen_flat = 0
```

A flask of water today:

```json
{ "Liquid": "water", "Capacity": 5, "Remaining": 5, "Poisoned": false }
```

Becomes:

```
liquid_id = 1     # FK to Liquids.id where name = 'water'
liquid_capacity = 5
liquid_remaining = 5
liquid_poisoned = false
```

A portal to room (30, 45) today:

```json
{ "Destination": 3045 }   # legacy vnum: zone 30 × 100 + id 45
```

Becomes:

```
portal_dest_zone = 30
portal_dest_room = 45
```

## Open questions

1. **Per-type tables vs per-type columns.** Alternative: a separate
   `WeaponData(object_zone, object_id, …)` table per ObjectType,
   with `Objects` carrying just the generic columns. More normalized
   but more joins. I default to "columns on `Objects`" because every
   read path knows the type already from `r#type`, so the conditional
   nullability isn't wasteful.
2. **Wear flags vs `armor_pct`/`armor_flat`.** Today armor items
   have `wear_flags` (where they sit) but no actual mitigation
   numbers — they exist as decoration. The combat redesign needs
   armor numbers per-item. Add the columns now, populate from a
   default "level-tier" table in the migration?
3. **Drink-fountain remaining.** I propose `null` = bottomless
   (matches the runtime fountain treatment from commit 4362f71).
   Alternative: `-1`. Null reads better; null vs 0 is unambiguous.
4. **`weapon_class` enum granularity.** SWORD, AXE, DAGGER, MACE,
   STAFF, SPEAR, BOW, CROSSBOW, WHIP, CLAW, FIST? Or simpler ONE_HAND
   / TWO_HAND / RANGED / NATURAL? I default to the bigger list since
   `weapon_required` on abilities (see abilities.md) wants
   class-specific gating ("backstab requires DAGGER").
5. **Should `pen_*` live on the weapon proto or only on the wielder?**
   Today the combat math reads it from the attacker. Cleaner to read
   from the equipped weapon (so different weapons confer different
   penetration). Recommendation: weapon-side, summed with any
   wielder-side bonus from active effects.
