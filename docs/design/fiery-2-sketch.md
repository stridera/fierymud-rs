# Fiery 2.0 sketch — combat / abilities / effects

**Purpose:** eyeball-test for "incremental cleanup vs. rewrite."
Designs the combat / abilities / effects subsystem fresh, applying
specific answers to the Tier 1 grilling targets from
[`schema-design-review.md`](./schema-design-review.md). The goal is
not to be the final answer — it's to give the user a concrete
artifact to judge whether the gap between "current + Tier 1
cleanup" and "fresh design" is bigger than the prose review made it
sound.

**Companion to** the design review and the cleanup plan; not yet a
migration target.

## Tier 1 answers encoded in this sketch

These are the assumptions baked in. Each one corresponds to a
grilling target the user hasn't formally answered yet — I've picked
*one* answer per target so the sketch is concrete. Final answers can
differ.

| Target | Decision encoded here |
|---|---|
| G-A1 (resistance representation) | Typed `resistance` table, polymorphic by owner. JSONB resistance columns removed. |
| G-D1 (Ability single-table inheritance) | Class-table inheritance — `Ability` base + `Spell` / `Skill` / `Song` / `Chant` sidecars (1:1, mirror of existing `AbilityMessages` pattern). |
| G1 (stringly-typed discriminators) | `AbilityKind` and `EffectKind` enums. |
| G-D2 (ClassAbilities + ClassSkills) | Merged into single `ClassAbility` junction (nullable `circle` or `minLevel` per Ability.kind). |
| G-D3 (Race enum overload) | Out of scope for this cluster, but noted: `Race` would split into `PlayableRace` + `CreatureKind`. |
| G-C1 (unify effect junctions) | **NOT unified.** Kept separate per carrier (typed FKs win over polymorphism with nullable cols). Asymmetry fix: `RoomEnvironmentalEffect` gains `strength` + `modifierData` for shape parity. |
| G-E1 / G-E2 (naming) | Singular model names + snake_case table names. |

## Enums (new)

```prisma
enum AbilityKind {
  SPELL
  SKILL
  SONG
  CHANT
}

enum EffectKind {
  MODIFY        // backs StatModifier; modifier_data = { target, amount }
  STATUS        // named gameplay state (INVISIBLE, DROWNING, PARALYZED)
  DAMAGE        // direct damage application
  HEAL
  ROOM          // room-level effect (wall spells)
  STUN
  KNOCKDOWN
  CLEANSE
  DISPEL
  TRANSFORM
  SUMMON
  TELEPORT
  // ... full list mirrors the 28 effectType values in current data
}

enum ResistanceOwner {
  CHARACTER
  MOB
  OBJECT
  CHARACTER_CLASS
  RACE
}

enum RestrictionRefKind {
  CLASS
  RACE
  ALIGNMENT
  LEVEL
  ITEM
  EFFECT
}
```

## `ability` — base + kind sidecars

```prisma
model Ability {
  id        Int         @id @default(autoincrement())
  name      String      // display with color codes
  plainName String      @unique @map("plain_name")
  kind      AbilityKind

  description    String?
  minPosition    Position    @default(STANDING) @map("min_position")
  violent        Boolean     @default(false)
  castTimeRounds Int         @default(1) @map("cast_time_rounds")
  cooldownMs     Int         @default(0) @map("cooldown_ms")
  inCombatOnly   Boolean     @default(false) @map("in_combat_only")
  combatOk       Boolean     @default(true) @map("combat_ok")
  isArea         Boolean     @default(false) @map("is_area")
  targetScope    TargetScope @default(SINGLE) @map("target_scope")
  isMagical      Boolean     @default(true) @map("is_magical")
  isToggle       Boolean     @default(false) @map("is_toggle")
  schoolId       Int?        @map("school_id")
  notes          String?
  tags           String[]    @default([])
  luaScript      String?     @map("lua_script") @db.Text

  contestedVisibility Boolean @default(false) @map("contested_visibility")
  visibilityCheck     String? @map("visibility_check")

  // Kind sidecars (1:1 — exactly one populated based on `kind`)
  spell Spell?
  skill Skill?
  song  Song?
  chant Chant?

  // Unchanged from current
  school              AbilitySchool?            @relation(fields: [schoolId], references: [id])
  effects             AbilityEffect[]
  components          AbilityComponent[]
  damageComponents    AbilityDamageComponent[]
  savingThrows        AbilitySavingThrow[]
  targeting           AbilityTargeting?
  messages            AbilityMessages?
  restrictions        AbilityRestriction[]      // ← was Json[]; now typed table
  // ... junctions to characters, races, classes, etc.

  createdAt DateTime @default(now()) @map("created_at")
  updatedAt DateTime @updatedAt @map("updated_at")

  @@map("ability")
}

model Spell {
  abilityId        Int          @id @map("ability_id")
  sphere           SpellSphere
  damageType       ElementType? @map("damage_type")
  pages            Int?
  memorizationTime Int          @default(0) @map("memorization_time")
  questOnly        Boolean      @default(false) @map("quest_only")
  humanoidOnly     Boolean      @default(false) @map("humanoid_only")

  ability Ability @relation(fields: [abilityId], references: [id], onDelete: Cascade)

  @@map("spell")
}

model Skill {
  abilityId Int           @id @map("ability_id")
  category  SkillCategory
  type      SkillType

  ability Ability @relation(fields: [abilityId], references: [id], onDelete: Cascade)

  @@map("skill")
}

model Song { /* same shape as Skill — per-kind knobs */
  abilityId Int @id @map("ability_id")
  ability   Ability @relation(fields: [abilityId], references: [id], onDelete: Cascade)
  @@map("song")
}

model Chant { /* same */
  abilityId Int @id @map("ability_id")
  ability   Ability @relation(fields: [abilityId], references: [id], onDelete: Cascade)
  @@map("chant")
}

// Replaces AbilityRestrictions.requirements Json[]
model AbilityRestriction {
  id        Int                @id @default(autoincrement())
  abilityId Int                @map("ability_id")
  refKind   RestrictionRefKind @map("ref_kind")
  refInt    Int?               @map("ref_int")    // for level, classId, itemId
  refString String?            @map("ref_string") // for race / alignment
  ability   Ability            @relation(fields: [abilityId], references: [id], onDelete: Cascade)

  @@map("ability_restriction")
}
```

## `effect` — `kind` is now an enum

```prisma
model Effect {
  id            Int        @id @default(autoincrement())
  name          String     @unique
  description   String?
  kind          EffectKind
  tags          String[]
  defaultParams Json       @default("{}") @map("default_params") // KEEP: builder authoring shape
  categoryId    Int?       @map("category_id")

  category ToolboxCategory? @relation(fields: [categoryId], references: [id])

  // Action restrictions, lua hooks, etc — all unchanged
  preventsSpeaking Boolean @default(false) @map("prevents_speaking")
  preventsCasting  Boolean @default(false) @map("prevents_casting")
  preventsMovement Boolean @default(false) @map("prevents_movement")
  onApply          String? @map("on_apply")
  onTick           String? @map("on_tick")
  onRemove         String? @map("on_remove")
  delaySeconds    Int?    @map("delay_seconds")
  tickIntervalSec Int?    @map("tick_interval_sec")

  // Junctions stay separate (G-C1: NOT unified)
  objectEffects     ObjectEffect[]
  mobDefaultEffects MobDefaultEffect[]
  characterEffects  CharacterEffect[]
  raceEffects       RaceEffect[]
  roomEffects       RoomEnvironmentalEffect[]
  consumableEffects ConsumableEffect[]

  @@map("effect")
}
```

## `resistance` — typed table replaces 4 JSONB columns

```prisma
model Resistance {
  id        Int             @id @default(autoincrement())
  ownerType ResistanceOwner @map("owner_type")

  // Exactly one carrier-key set per ownerType (mirrors the
  // composite-key pattern of carrier tables)
  characterId  String? @map("character_id")
  mobZoneId    Int?    @map("mob_zone_id")
  mobId        Int?    @map("mob_id")
  objectZoneId Int?    @map("object_zone_id")
  objectId     Int?    @map("object_id")
  classId      Int?    @map("class_id")
  race         Race?   // Race enum used as-is for race-bound resistances

  element ElementType
  value   Int // -100 (absorb→heal) to 200 (double damage)

  // Convenient FKs back to owners
  character Character?      @relation(fields: [characterId], references: [id], onDelete: Cascade)
  mob       Mob?            @relation(fields: [mobZoneId, mobId], references: [zoneId, id], onDelete: Cascade)
  object    Object?         @relation(fields: [objectZoneId, objectId], references: [zoneId, id], onDelete: Cascade)
  class     CharacterClass? @relation(fields: [classId], references: [id], onDelete: Cascade)
  raceData  RaceTable?      @relation(fields: [race], references: [race], onDelete: Cascade)

  @@index([ownerType, characterId])
  @@index([ownerType, mobZoneId, mobId])
  @@index([ownerType, objectZoneId, objectId])
  @@index([ownerType, classId])
  @@index([ownerType, race])

  @@map("resistance")
}
```

This collapses `ObjectResistance` AND the four JSONB columns into one
table. Querying "all resistances for this character" is one indexed
read; updating one element is one row write. Authoring in Muditor
becomes "row editor" instead of "JSON editor."

The nullable-carrier-id columns look like the same problem as the
Quest polymorphism mess — but here they're **homogeneous by intent**
(every owner is "a carrier of resistances"), not heterogeneous-by-
discriminator (where the discriminator changes the whole row's
meaning). The shape is the same; the *meaning of the shape* is what
makes it healthy vs. muddled. (Could also be done with a CHECK
constraint per ownerType for strict invariants.)

## Effect attachments — kept separate, asymmetry fixed

Decision: don't unify into `EffectAttachment` polymorphic table.
Reason: typed FKs (with proper cascade) are worth more than a 5→1
table consolidation. The current shape is fine; just fix the
asymmetry.

```prisma
// ObjectEffect, MobDefaultEffect, CharacterEffect, RaceEffect:
// shape unchanged from current schema

model RoomEnvironmentalEffect {
  roomZoneId   Int  @map("room_zone_id")
  roomId       Int  @map("room_id")
  effectId     Int  @map("effect_id")

  // ← NEW: parity with the other 4 effect-attachment tables
  strength     Int  @default(1)
  modifierData Json @default("{}") @map("modifier_data")

  room   Room   @relation(fields: [roomZoneId, roomId], references: [zoneId, id], onDelete: Cascade)
  effect Effect @relation(fields: [effectId], references: [id], onDelete: Cascade)

  @@id([roomZoneId, roomId, effectId])
  @@map("room_environmental_effect")
}
```

## Class abilities — merged

```prisma
model ClassAbility {
  id             Int @id @default(autoincrement())
  classId        Int @map("class_id")
  abilityId      Int @map("ability_id")

  // Per-kind context (read based on Ability.kind):
  circle         Int?  // SPELL/SONG/CHANT — required circle
  minLevel       Int?  @map("min_level")  // SKILL — level the class first learns it
  proficiencyCap Int  @default(100) @map("proficiency_cap")

  characterClass CharacterClass @relation(fields: [classId], references: [id], onDelete: Cascade)
  ability        Ability        @relation(fields: [abilityId], references: [id], onDelete: Cascade)

  @@unique([classId, abilityId])
  @@map("class_ability")
}
```

Nullable `circle`/`minLevel` is mild noise; `Ability.kind` tells the
reader which to use. Beats two parallel tables.

## What stays unchanged (~70% of the cluster)

Just to be explicit: most of this subsystem isn't redesigned.

- **All combat columns** on Character / Mob (`accuracy`, `attackPower`, `spellPower`, `penetration*`, `evasion`, `armorRating`, `damageReductionPercent`, `soak`, `hardness`, `wardPercent`, `perception`, `concealment`)
- **`AbilityEffect`** junction (linking abilities to effects with order/trigger/chance)
- **`AbilityComponent`** (material spell components)
- **`AbilityDamageComponent`** (multi-element damage splits)
- **`AbilitySavingThrow`**
- **`AbilityTargeting`** sidecar
- **`AbilityMessages`** sidecar
- **`AbilitySchool`** catalog
- **`ToolboxCategory`**
- **`SpellSlotProgression`** + **`ClassAbilityCircles`** + **`RaceSpellSlotBonus`**
- **`ObjectEffect`**, **`MobDefaultEffect`**, **`CharacterEffect`**, **`RaceEffect`** (kept separate, kept shape — only `RoomEnvironmentalEffect` changes)
- **`CharacterAbility`**, **`MobAbility`**, **`RaceAbility`**, **`ObjectAbility`** junctions (each with its own surrounding context)
- **`ConsumableEffect`** two-arm polymorphism (still fine)
- **`SaveType`**, **`ElementType`**, **`SpellSphere`**, **`SkillCategory`**, **`SkillType`**, **`TargetScope`**, **`TargetType`** enums
- **`Position`**, **`MovementMode`**, **`PositionData`** model

## Net delta vs. current schema

**New tables (6):**
- `spell`, `skill`, `song`, `chant` — kind sidecars under `ability`
- `ability_restriction` — replaces `AbilityRestrictions.requirements Json[]`
- `resistance` — replaces 4 JSONB columns + `ObjectResistance` table

**Modified tables (8):**
- `ability` (loses `abilityType String`, gains `kind AbilityKind`; spell-only columns moved to `spell` sidecar)
- `effect` (loses `effectType String`, gains `kind EffectKind`)
- `class_ability` (merges current `ClassAbilities` + `ClassSkills`)
- `room_environmental_effect` (adds `strength` + `modifier_data` for parity)
- `character` (drops `resistances` JSON column; reads from `resistance` table)
- `mob` (same)
- `character_class` (same)
- `race_table` (same)

**Dropped tables (1):**
- `ObjectResistance` — folded into the unified `resistance` table

**Unchanged:** ~20 tables in this cluster, including all the
1:N / 1:1 sidecars and all the catalogs.

## Importer burden (fierylib)

If we executed this fresh-design path:

| Importer area | Effort |
|---|---|
| Ability split (kind + sidecar emission) | Moderate — one branch per kind, mostly mechanical |
| Resistance reshape (4 JSON owners → rows) | Moderate — already understood from current cleanup remap pattern |
| `class_ability` unified emission | Trivial |
| Other importer changes | Same as current cleanup plan covers |

Roughly comparable to the cleanup plan's import work, slightly more
mechanical and slightly more total surface.

## Verdict

The fresh sketch is **~75% identical** to the current schema for
this cluster. 6 new tables, 8 modified ones, 1 dropped — out of
roughly 30 tables in the combat/abilities/effects neighborhood.

Crucially: **every meaningful change in this sketch is in the Tier 1
grilling queue.** There's no foundational decision a fresh design
would make that the incremental path can't reach. The vision-fit is
already there; the deltas are about *expressing* that vision with
tighter invariants.

If we did the Tier 1 grilling and shipped those migrations, the
existing schema would converge on this sketch. The end states are
the same.

**The remaining question is process, not design.** Three ways to get
there:

1. **Incremental** (default per the design review): grilling round → migration → next grilling round. ~hours per Tier 1 target. Each migration is bounded and testable. Content is re-imported via fierylib remaps as you go.
2. **Big-bang rewrite**: design the full Fiery 2.0 schema in one pass, port fierylib to emit it, full reset + reimport. ~weeks of design + porting + validation. One big chunk; either fully done or not.
3. **Hybrid**: do the Tier 1 grilling to lock decisions, then ship Tier 1 as a single batched migration (rather than one-at-a-time). Skips the cadence overhead while keeping the design discipline.

The hybrid is probably the right shape for someone solo'ing this:
all the design clarity of (2) with most of the deliberation safety
of (1).

## How to use this doc

When the user wakes up: skim the diff sections (new / modified /
unchanged tables), compare against the mental image of the current
schema, and decide whether the gap is bigger than incremental
cleanup can close. If the diff feels small enough to be "Tier 1
grilling outcome" — go incremental. If it feels meaningfully
different — flag what specifically, and we adjust the plan.
