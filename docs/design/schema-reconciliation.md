# Schema Reconciliation

**Status:** proposal — awaiting review.

## Design intent

This is the cleanup pass: every schema duplicate, dead column, and
"two ways to spell the same concept" gets resolved. Independent of the
combat/effects/objects redesigns — those add new columns; this one
removes the old ones.

Each item below is a discrete migration. Most are one or two lines.

## Drop dead duplicates

### `Triggers.mobZoneId` / `mobObjectId` / `objectZoneId` / `objectId`

Direct FK columns on `Triggers`. Runtime exclusively reads the
junction tables (`MobTriggers` / `ObjectTriggers` / `RoomTriggers`,
loader.rs:836-855). The direct FK shape was an early attempt that
got superseded; the columns survive as silent dead weight.

**Action:** drop all four columns.

### `Triggers.variables Json @default("{}")`

Designed for "Lua trigger persistent vars across calls" — but the
intended home is the separate `EntityVariables` table (which itself
isn't loaded at runtime today; see latent-content section below).
The `Triggers.variables` field is unread.

**Action:** drop the column.

### `Characters.raceType String` (shadows `Characters.race Race`)

Two columns for the same concept. The legacy text version is from
before the enum landed; `race_type` is "human" / "elf" while `race`
is the enum HUMAN / ELF. Runtime reads the enum.

**Action:** drop `raceType`.

### `Characters.playerClass String?` (shadows `Characters.classId Int?`)

Same pattern: legacy text vs typed FK. Runtime reads `classId`.

**Action:** drop `playerClass`.

### `MobCarrying` (overlaps `MobResetEquipment` and `EquipmentSets`)

Three different schemas for "this mob spawns with these items":
1. `MobResetEquipment` (used by the loader today)
2. `EquipmentSets` + `MobEquipmentSets` (junction-table version,
   never loaded)
3. `MobCarrying` (yet another junction, never loaded)

**Action:** keep `MobResetEquipment`. Drop `EquipmentSets`,
`MobEquipmentSets`, `MobCarrying`. fierylib re-importer should
emit only `MobResetEquipment` rows going forward.

### `ObjectAffects` (overlaps `ObjectEffects` + `ObjectResistance`)

Legacy CircleMUD "+2 STR ring" model: rows of
`(location, modifier)` pairs. Replaced cleanly by the modern shape:
`ObjectEffects` (effect-system buffs the item grants) +
`ObjectResistance` (per-element resistance the item confers). Runtime
reads the modern tables.

**Action:** drop `ObjectAffects`. Re-import script translates any
existing legacy rows into `ObjectEffects(modify, target=str,
amount=2)` etc.

## Promote string columns to typed enums

### `MobResetEquipment.wearLocation String?` → `WearLocation`

Today the column is free text; `Slot::from_label` accepts an
expanding list of synonyms ("NECK_1" / "NECK_2" / "EARS" / "EAR" /
"FINGER_R" / "FINGER_L"). Promote to a typed enum that mirrors
`Slot`. Same enum should also replace
`CharacterItems.equippedLocation String?`.

**Action:** add `WearLocation` enum, migrate both columns.

### `AbilitySavingThrow.onSaveAction Json` → `SaveResult` enum

Single-value JSON storing one of `NEGATE` / `HALF_DURATION` /
`HALF_DAMAGE` / `NO_EFFECT`. Type-check it.

**Action:** add `SaveResult` enum; migrate the column.

## Latent content tables — wire or remove

These tables exist in the schema but are not loaded at runtime. Each
needs a decision: ship the feature or drop the table.

| Table(s) | Use | Recommendation |
|---|---|---|
| `EntityVariables` | Lua trigger persistent vars across calls | **wire** (small loader pass; trigger system already wants this) |
| `CombatMessage`, `PositionMessage`, `SystemMessage`, `LoginMessage`, `SystemText` | Builder-authored flavor text replacing hardcoded strings | **wire** (high content-leverage, modest loader work) |
| `HelpEntry` | In-game help articles | **wire** (already a `help` command; just needs DB-backed lookup) |
| `PlayerToggle` | Configurable per-player flag set | **drop** — `PlayerFlag` enum + `PlayerFlags` component already covers this |
| `GameConfig` | k/v runtime config | **wire** as the home for migrated `pub(crate) const` values |
| `Deployment` | Deployment metadata | **drop** — operational concern, not a runtime table |
| `ChangeLogs`, `AuditLogs` | Muditor + admin audit trails | **keep, write-only at runtime** (already the case) |
| `Quest`, `QuestObjective`, `QuestDialogue`, `CharacterQuest`, `CharacterQuestObjective` | Quest system | **wire incrementally** — see open backlog |
| `Shapechange*`, `AbilityShapechangeForm` | Druid form transitions | **drop until shapechange is a real feature** |
| `ShopHours`, `ShopRooms`, `ShopAbilities` | Time-gated / multi-room shop logic | **drop** — Shops as currently shipped don't need it |

## Enum bloat — fold or wire

### `Sector` planar variants

`ASTRALPLANE`, `AIRPLANE`, `FIREPLANE`, `EARTHPLANE`, `ETHEREALPLANE`,
`AVERNUS`. Currently no runtime behavior differentiates these from
`OUTDOOR` / `CAVE`. Either:

- **(A)** Wire each planar sector to a default
  `RoomEnvironmentalEffect` (FIREPLANE applies fire damage tick;
  ASTRALPLANE applies confusion). Real mechanical difference.
- **(B)** Fold all six into a single `PLANAR` value with a
  builder-authored `subtype` field for flavor.

Recommendation: **(A)**. The `RoomEnvironmentalEffect` table exists
specifically for this; planar sectors are the use case.

### `Composition` enum

13 variants for character body materials (`FLESH`, `IRON`, `GAS`,
`PLANT`, `EARTH`, `MAGIC`, `ETHER`, `STONE`, `MINERAL`, `CRYSTAL`,
`METAL`, `LIQUID`, `BONE`). Nothing in code reads it today.

Recommendation: **drop** until a feature actually needs it.
Re-introduce when (e.g.) a "shatter stone golem" ability needs a
`composition_required` filter.

### `MagicAffinity`

Declared, never read.

Recommendation: **drop**.

### `Composition` and `Size` overlap

`Mobs.size` and `Characters` both have a size dimension; meanwhile
`baseSize` / `currentSize` are integers. Pick one representation
(enum `Size` with `TINY`/`SMALL`/`MEDIUM`/`LARGE`/`HUGE`/`GARGANTUAN`
is conventional) and drop the integer columns.

## Body-metric column dust

`Characters.height`, `weight`, `baseHeight`, `baseWeight`, `baseSize`,
`currentSize` — six columns, none read at runtime. `Races` mirrors
some of them (`maleHeight*`, `femaleHeight*`, `maleWeight*`,
`femaleWeight*`).

Decision needed: are these used for character creation flavor (in
which case wire them to a one-time roll on creation and surface in
`description`)? Or pure flavor / cut?

Recommendation: **wire on creation, expose via `description`, drop
`current_*` runtime mutation**. Body changes via
shape-change effects modify a `SizeOverride` component, not the
persisted column.

## `Mobs` column audit

Per the audit notes, `Mobs` ships ~50 columns; runtime reads ~21.
Resolve column-by-column with the combat redesign:

- **Keep**, used by combat redesign: `accuracy`, `evasion`,
  `attackPower`, `spellPower`, `penFlat`, `penPct`, `armorRating` →
  rename to `armor_pct`, `damageReductionPercent` → fold into
  `armor_pct`, `soak` → rename to `armor_flat`, `wardPercent` →
  `ward_pct`, `hardness`, `concealment`, `perception`,
  `resistances`, `lifeForce`, `damageType`, six core attributes.
- **Drop**, replaced: `armorClass`, `hitRoll`, `damageRoll`, the
  four `*_dice_*` columns.
- **Latent / decide**: `composition`, `stance`, `traits[]`,
  `riderPresenceMessage`, `aggressionFormula`, `activityRestrictions`,
  `estimatedHp`, `raceAlign`, `defaultMovementMode`, `movementMode`,
  `move`. Wire what becomes mechanically meaningful; drop the rest.

## Migration ordering

To avoid runtime breakage:

1. **First batch — additions** (no breakage):
   - Add `Ability.staminaCost`, `Ability.manaCost`, `Ability.targetScope`.
   - Add new `Mobs` / `Characters` / `Objects` columns from combat redesign.
   - Add `WearLocation` and `SaveResult` enums.
2. **Re-import** populates the new columns from existing legacy
   data.
3. **Runtime switches** to reading new columns.
4. **Second batch — removals**: drop legacy columns, dead enums,
   duplicate tables. Each in a separate migration so a rollback is
   surgical.

## Open questions

1. **fierylib re-import strategy.** Most of these migrations need
   data back-filled from the existing rows. Add a per-migration
   transformer in fierylib or one big "post-redesign re-import"
   script? I'd default to per-migration so each schema change is
   self-contained.
2. **`MobCarrying` deletion vs marking deprecated.** Drop it
   outright vs leave the table empty for one release cycle in case a
   downstream consumer (Muditor preview?) still reads it. Probably
   drop — there's only one consumer chain.
3. **`PlayerToggle` cleanup.** Are there any toggles in the current
   `PlayerToggle` rows that aren't covered by `PlayerFlag` enum
   variants? If yes, those need to land on the enum first.
