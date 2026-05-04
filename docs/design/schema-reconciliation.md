# Schema Reconciliation

**Status:** locked except where noted (review pass 1, 2026-05-03).

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

### `Ability.classes Json?` (duplicates `ClassAbilities`)

Today the per-class circle assignment for a spell lives in two
places: `Ability.classes` JSON shorthand
(`{"Pyromancer": 7, "Sorcerer": 3}`) AND the normalized
`ClassAbilities(class_id, ability_id, circle, proficiency_cap)`
junction table. Two representations, content authors edit one, the
other goes stale.

**Action:** drop `Ability.classes`. `ClassAbilities` is the
source of truth — it carries `circle` plus `proficiency_cap`
which the JSON shorthand can't express.

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

**Drop until a feature actually needs it.** When the enum goes,
the columns referencing it go too:

- `Mobs.composition` → drop
- `Races.defaultComposition` → drop
- `Characters.composition` (if it exists) → drop

Re-introduce the enum when (e.g.) a "shatter stone golem" ability
needs a `composition_required` filter — at that point the columns
come back simultaneously, not staggered.

### `MagicAffinity`

Declared, never read.

Recommendation: **drop**.

### `Size` representation

Single source of truth: the existing `Size` enum on `Mobs` and
`Races` (`TINY`/`SMALL`/`MEDIUM`/`LARGE`/`HUGE`/`GIANT`/`GARGANTUAN`/
`COLOSSAL`/`MOUNTAINOUS`/`TITANIC`). `Characters` doesn't carry a
size column directly — body size derives from `Race.defaultSize`,
which already covers character creation needs. The `baseSize` /
`currentSize` integer columns drop per the body-metric section
below.

## Body-metric column dust

`Characters.height`, `weight`, `baseHeight`, `baseWeight`, `baseSize`,
`currentSize` — six columns, none read at runtime. `Races` mirrors
some of them (`maleHeight*`, `femaleHeight*`, `maleWeight*`,
`femaleWeight*`).

**Resolution:**

- **Keep `Characters.height` and `Characters.weight`** as immutable-after-creation
  flavor columns. Character-creation flow rolls them once from the
  Race's gender-keyed range and writes the result. Score sheet renders
  them on a "Height: 5'10″ Weight: 180 lbs" line — same idea as the
  Played: / Last login: pattern already shipping.
- **Drop `baseHeight`, `baseWeight`, `baseSize`, `currentSize`.** The
  `base*` columns shadowed `height`/`weight`; the `current*` integer
  columns wanted to track shape-change overrides which now belong on
  a runtime `SizeOverride` ECS component instead.
- **Keep the `Race` range columns** (`male/femaleHeight*`,
  `male/femaleWeight*`) — character creation reads them. Drop only
  the per-character mirror columns.

Note: `description` is authored prose for `examine` output, not a
field for system-generated stat dumps. Surface body metrics on
score, leave `description` alone.

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

0. **Pre-flight audit** (no schema change). Run a diagnostic SQL pass
   to surface the cases the cleanup migrations need to handle:
   - `SELECT DISTINCT toggle_name FROM PlayerToggle` — list any rows
     whose names aren't covered by `PlayerFlag` enum variants. Each
     uncovered toggle either lands on the enum first or gets
     promoted to a dedicated column before `PlayerToggle` drops.
   - `SELECT DISTINCT element FROM ObjectAffects WHERE NOT migratable`
     — any modifier that doesn't translate cleanly into
     `ObjectEffects(modify, target=…)` needs a hand-written
     `ObjectEffects` row in the re-import.
   - `SELECT name, current_size FROM Characters WHERE current_size != base_size`
     — characters mid-shapechange when the migration runs need
     their `current_size` captured into a transient
     `SizeOverride` runtime row before the column drops.
1. **First batch — additions** (no breakage):
   - Add `Ability.staminaCost`, `Ability.manaCost`, `Ability.targetScope`,
     `Ability.is_magical Bool @default(true)`.
   - Add new `Mobs` / `Characters` / `Objects` columns from combat redesign.
   - Add `Mobs.default_posture Posture` and `Characters.is_frozen Bool`
     from posture-and-lifestate.md.
   - Add `WearLocation`, `SaveResult`, `ObjectAbilityTrigger`, and
     `DamageType` enums.
2. **Re-import** populates the new columns from existing legacy
   data. **Runtime offline during this phase** — the world process
   reads stale legacy columns while fierylib writes new ones, so a
   stop-the-world window is the simplest invariant. Coordinate with
   ops; expect single-digit-minute downtime for the import.
3. **Runtime switches** to reading new columns. Deploy + restart;
   legacy columns become dead reads.
4. **Second batch — removals**: drop legacy columns, dead enums,
   duplicate tables. Each in a separate migration so a rollback is
   surgical.

## Decisions locked (review pass 1, 2026-05-03)

| Question | Locked |
|---|---|
| fierylib re-import strategy | **Per-migration transformer.** Each schema change ships its own back-fill so individual migrations stay self-contained and reversible. The "one big script" alternative bets the whole cleanup on a single run; per-migration matches the additions / re-import / switch / removals phasing. |
| `MobCarrying` deletion | **Drop outright.** No deprecation cycle — Muditor doesn't read it (per loader.rs audit), there's no second consumer to wait on, and the table sits empty in the schema noise budget otherwise. |
| `PlayerToggle` audit | **Promoted to migration step 0.** Pre-flight `SELECT DISTINCT toggle_name FROM PlayerToggle` runs before phase 1; uncovered names become PlayerFlag variants or dedicated columns before the table drops. Not a runtime-time question. |
| Body-metric columns | **Keep `height` / `weight` on Characters; drop `base*` / `current*` mirrors.** Roll once at character creation from Race ranges; surface on score. Shape-change overrides live in a runtime `SizeOverride` ECS component. |
| `description` field reuse | **Keep authored prose only.** Don't append system-generated body metrics; surface those on score. |
| `Composition` orphans | **Drop the enum and every column that references it together** (`Mobs.composition`, `Races.defaultComposition`, `Characters.composition` if present). No staggered drops. |

## Remaining open questions

None blocking the migration. Add tracker rows here if the audit
phase surfaces uncovered cases (PlayerToggle names, ObjectAffects
modifiers, mid-shapechange characters) that need a one-off
mitigation step.
