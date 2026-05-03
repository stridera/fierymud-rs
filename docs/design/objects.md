# Objects

**Status:** locked (review pass 1, 2026-05-03).

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
| `base_damage` | Int? | WEAPON, MISSILE | replaces Hit Dice (see [combat.md](combat.md)). MISSILE rows carry an optional small bonus (default 0) added on top of the bow's base damage. |
| `damage_type` | DamageType? | WEAPON, MISSILE | enum, see [damage-types.md](damage-types.md) |
| `weapon_class` | WeaponClass? | WEAPON, INSTRUMENT | full enum: SWORD, AXE, DAGGER, MACE, STAFF, SPEAR, BOW, CROSSBOW, SLING, FLAIL, POLEARM, WHIP, CLAW, FIST, plus instrument variants — see "Weapon class enum" below. |
| `requires_ammo` | MissileClass? | WEAPON (ranged only) | If set, every swing consumes one matching MISSILE from inventory. `null` for melee weapons. |
| `missile_class` | MissileClass? | MISSILE | ARROW, BOLT, DART, STONE, BULLET. Pairs with `requires_ammo` on bows/crossbows/slings/blowguns. |
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
| `pen_pct` | Int? | WEAPON | armor-pierce %. Lives on weapon-side; wielder bonuses come from active modify-effects. |
| `pen_flat` | Int? | WEAPON | armor-pierce flat |

### Drop

- `Object.values` JSONB column.
- The legacy "Pages doubles as Board.id" and "Destination is a vnum"
  conventions.

## ObjectType rationalization

`ObjectType` shrinks from 38 variants to ~28. Several legacy types
encoded *behavior* that's better expressed as "type X with effect Y."
Dropped types and their migration target:

| Drop | Why | Migration |
|---|---|---|
| `FIREWEAPON` | `damage_type = FIRE` on regular WEAPON + on-hit `burning` STATUS via ObjectEffects covers it | Each FIREWEAPON → WEAPON with `damage_type = FIRE`; ObjectEffects row → `burning` Effect catalog row. |
| `WORN` | Distinction from ARMOR was "wearable but no mitigation." Same shape, just `armor_pct = 0` | Each WORN → ARMOR with `armor_pct = 0`. |
| `TRAP` | Triggers on items / rooms handle trap mechanics; no special type needed | Trap items either rebuild as triggered items, or convert to OTHER if decorative. |
| `WINGS` | Wings are a slot, not a type. New `Slot::Wings` + `WearFlag::Wings`; flight is an effect. | Each WINGS → ARMOR with `wear_flags = [WINGS]`; ObjectEffects row → `fly` STATUS. |
| `PERFUME` | Charisma buff via ObjectEffects (worn) or consumable POTION-style with duration | Re-author per item. |
| `DISGUISE` | Pure effect mechanic — neither a type nor a slot. Disguise items are normal worn (Face / Head / About) with a `disguised_as_*` STATUS via ObjectEffects. | Each DISGUISE → ARMOR with appropriate wear_flag + ObjectEffects → disguise STATUS. |
| `POISON` | Substance applied to weapons via skill/spell, or consumable potion with damage-on-self | Re-author. POISON → POTION (consumable) or OTHER (substance) + per-content effects. |
| `BOAT` | Subset of VEHICLE | BOAT → VEHICLE; add `vehicle_class` enum if water-only matters mechanically. |

Kept (with notes):

| Type | Notes |
|---|---|
| `WEAPON` | Now carries `base_damage`, `damage_type`, `weapon_class`, `pen_pct`, `pen_flat`, `requires_ammo` |
| `ARMOR` | Now carries `armor_pct`, `armor_flat` |
| `MISSILE` | Now carries `damage_type`, `missile_class`, optional `base_damage` bonus |
| `INSTRUMENT` | Now carries `weapon_class` (extended enum covers instruments) |
| `LIGHT` | `light_capacity` + `light_remaining` |
| `DRINKCONTAINER`, `FOUNTAIN` | `liquid_id` + `liquid_capacity` + `liquid_remaining` + `liquid_poisoned` |
| `PORTAL` | `portal_dest_zone` + `portal_dest_room` |
| `BOARD` | `board_id` |
| `CONTAINER` | `container_capacity` + lock-key columns |
| `KEY` | No data beyond name; pairs with CONTAINER's lock columns |
| `FOOD` | `food_hours` |
| `POTION`, `SCROLL`, `WAND`, `STAFF` | Use `ObjectAbilities` table for spell bindings (already exists) |
| `SPELLBOOK` | `Ability.pages` already drives scribe cost; `ObjectSpellbookEntries` (or similar junction) carries which spells the book teaches — schema-level decision tracked in [schema-reconciliation.md](schema-reconciliation.md) |
| `TOUCHSTONE` | No extra columns; runtime `cmd_touch` reads the type label |
| `VEHICLE` | Absorbs former BOAT; optional `vehicle_class` enum (LAND / WATER / AIR) |
| `MONEY`, `TREASURE` | `Objects.cost` is the value |
| `CORPSE` | `decompose_timer` + a `Corpse` runtime component carrying contents |
| `KIT` | Crafting-tool stub; add columns when crafting lands |
| `WALL`, `ROPE`, `PEN`, `NOTE`, `OTHER`, `TRASH`, `NOTHING` | Narrow content uses; existing columns suffice |

Net: 38 → 28 types. fierylib re-importer normalizes legacy data per
the migration table above.

## Slots — adding `Wings`

Three back-region slots, each with distinct semantics:

```
Slot enum:
  ...existing variants...
  About      # cloak, cape — across the body
  Wings      # NEW — back attachment (folded wings, harness)
  Hover      # orbiting accessory (rune, familiar, glowing sigil)
```

These don't conflict — a player can wear a cloak (About), wing
harness (Wings), and floating rune (Hover) simultaneously.

Add `WearFlag::Wings` so items can declare they fit the slot:

```
WearFlag enum:
  ...existing variants...
  Wings      # NEW — pairs with Slot::Wings
```

Items today flagged as ObjectType::WINGS get re-tagged at migration:
`ObjectType::ARMOR` + `wear_flags: [WINGS]` + an ObjectEffects row
pointing at the `fly` Effect catalog row.

## Missile / ranged combat — locked at option (A)

Same-room only, ammo-consuming. No spatial mechanics, no
adjacent-room targeting, no LOS. The simplest model that still
makes ranged content meaningful via class theming and the
ammo-economy gameplay loop.

### Wielding a ranged weapon

A WEAPON with `weapon_class IN (BOW, CROSSBOW, SLING, BLOWGUN)` and
`requires_ammo = ARROW | BOLT | STONE | DART` triggers the
ranged-attack path. The combat tick's swing-snapshot pre-pass
extends to:

1. Resolve attacker's wielded weapon proto.
2. If `requires_ammo` is set:
   - Find the first carried MISSILE matching `missile_class`.
   - If none: emit "you have no arrows" once, swing fizzles (no damage,
     no stamina drain, no follow-up engagement).
   - If found: consume one (decrement stack count, despawn instance
     when last is gone).
3. Damage = `weapon.base_damage * (1 + attack_power) * variance`
   plus an optional `missile.base_damage` flat bonus
   (an iron-tipped arrow adds +1 over a wooden one).
4. Run the standard armor / resist / hardness pipeline from
   [combat.md](combat.md).

### Why option (A)

- Reuses every existing combat path. Combat tick doesn't grow new
  cross-room concerns.
- Ammo economy is the differentiator: bows are "free" once you have
  arrows but you have to carry stacks. Melee weapons never run out.
- Ranged-themed classes (Ranger, Archer) get class flavor through
  `weapon_required: [BOW]` on their abilities, not through new
  spatial mechanics.

### What option (A) doesn't get us

- No "tank in front, archer in back" tactical layer. If you want
  cross-room combat ([options B / C in the original
  proposal](https://example/never)), that's a follow-up doc — adjacent-
  room targeting is substantial work and touches movement, LOS, group
  positioning, exit visibility. Not on the locked path today.

### Schema for missile content

```yaml
# A bow that requires arrows
ObjectType: WEAPON
weapon_class: BOW
base_damage: 10
damage_type: PIERCING
requires_ammo: ARROW
pen_pct: 0
pen_flat: 0

# An iron-tipped arrow stack
ObjectType: MISSILE
missile_class: ARROW
base_damage: 1                    # small bonus
damage_type: PIERCING             # mostly cosmetic; bow's type wins
```

When a player wields the bow and has arrows in inventory, every
combat-tick swing consumes one arrow and lands as a regular swing.
Out of arrows → fizzle. Switching weapons mid-combat works as today.

### Move (re-name only — semantics already exist)

- `Objects.cost` (already a column) — no change.
- `Objects.weight` — no change.
- `Objects.level` — no change.
- `Objects.wear_flags` — gains the `WINGS` variant.

## `WeaponClass` enum (extended for instruments)

```
WeaponClass:
  # Melee
  SWORD
  AXE
  DAGGER
  MACE
  STAFF
  SPEAR
  POLEARM
  FLAIL
  WHIP
  FIST           # brass knuckles, monk weapons
  CLAW           # natural weapon for some shapechange forms

  # Ranged
  BOW
  CROSSBOW
  SLING
  BLOWGUN

  # Instruments — bards / mystics wield these to perform
  LYRE
  FLUTE
  DRUM
  HORN
  LUTE
```

Both `WEAPON.weapon_class` and `INSTRUMENT.weapon_class` columns use
the same enum. `Ability.weapon_required: WeaponClass[]` filters
abilities by what's wielded — a song can require `[LYRE, FLUTE]`,
`backstab` can require `[DAGGER]`. Extending the enum later
(adding e.g. `RAPIER`, `SCYTHE`) is one migration line.

`MissileClass` enum is separate — covers ammo only:

```
MissileClass:
  ARROW
  BOLT
  STONE      # for slings
  DART       # for blowguns
  BULLET     # firearms — unused today, reserved
```

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

## Decisions locked (review pass 1, 2026-05-03)

| Question | Locked |
|---|---|
| Schema shape | **Columns on `Objects`**, nullable per type. Read path branches on `r#type`. Per-type satellite tables not worth the joins. |
| Armor item migration | **Migration script populates `armor_pct` / `armor_flat` from a level-tier default table** (1-10: 5/1; 11-30: 12/2; 31-60: 25/4; 61-90: 40/6; 90+: 55/9). Content authors hand-tune special items (artifacts, named gear) afterward. |
| Drink-fountain "remaining" | **`null` = bottomless.** Unambiguous against `0 = empty`. |
| `weapon_class` granularity | **Full list** — SWORD/AXE/DAGGER/MACE/STAFF/SPEAR/POLEARM/FLAIL/WHIP/FIST/CLAW + ranged + instrument variants. Single enum reused across WEAPON / INSTRUMENT columns. |
| `pen_*` location | **Weapon-side**, summed with wielder bonuses from active modify-effects. |
| Missile / ranged | **Option (A)** — same-room only, consume one MISSILE per swing, no spatial mechanics. Adjacent-room targeting deferred to a future ranged.md if ever. |
| Wings | **Drop type, keep slot.** Add `Slot::Wings` + `WearFlag::Wings`. Items use `wear_flags: [WINGS]` + ObjectEffects → `fly` STATUS. |
| Disguise | **Drop type AND slot.** Items use natural Face/Head/About slot + ObjectEffects → `disguised_as_*` STATUS. Magical disguise spells skip the item. |
| Type rationalization | **38 → 28 ObjectTypes.** Drops: FIREWEAPON, WORN, TRAP, WINGS, PERFUME, DISGUISE, POISON, BOAT. Each migrates to a kept type + appropriate ObjectEffects rows. |
| `Slot::Hover` vs `Slot::Wings` | **Both kept.** Hover is "orbiting accessory"; Wings is "back attachment." Distinct semantics, distinct slots. |

## Remaining open questions

1. **Spellbook content storage.** How does a spellbook know which
   spells it carries? Existing schema likely has `Spellbooks` /
   `SpellbookSpells` junction (verify). If not, add one. Tracked in
   [schema-reconciliation.md](schema-reconciliation.md).
2. **Vehicle subtype.** Add `vehicle_class LAND | WATER | AIR` if
   gameplay needs water-only or land-only restrictions. Defer until
   content actually wants it.
3. **Adjacent-room ranged combat (option B/C/D from the missile
   discussion).** Not in scope today. If we ever want a "tank in
   front, archer in back" tactical layer, write a dedicated
   `ranged.md` first — it touches movement, LOS, exit visibility,
   group positioning, and combat-tick cross-room targeting.
