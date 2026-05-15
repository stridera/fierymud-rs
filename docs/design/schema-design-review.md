# Schema design review (May 2026)

Macro-level review of `muditor/packages/db/prisma/schema.prisma`,
asking whether the **shape** of the schema serves the world we're
building. Companion to (not a replacement for) `database-audit.md`
(field-by-field cleanup) and `schema-cleanup-plan.md` (legacy
removal).

**Question being asked here:** *if we were designing this schema
fresh, knowing what we now know about the world, would it look like
this?*

## Status

DRAFT v2 (2026-05-15) — agent macro-pattern audit merged. ~20
grilling targets identified, organized by impact tier at the end.
Sections marked with ⚠️ are flagged as grilling targets.

## What kind of world this MUD is

Establishing the vision so we can judge schema-vs-vision fit.

- **Deep-RPG MUD** with D&D lineage (CircleMUD-descended) but a
  modernized combat axis (d100 accuracy/evasion contest, modern
  saves, multi-element damage, separate physical and ward
  mitigation).
- **Vancian casting** (spell circles + prepared slots), not mana.
  See [non-concepts in muditor/CONTEXT.md].
- **Class hierarchy** with subclasses (CharacterClass.parentClassId).
  Strong race system covering humanoids, fey, monstrous, dragons,
  dragonborn.
- **Builder-driven content**: world authoring happens in Muditor (web
  editor) with the schema as the contract. Builders should *rarely
  need a code change* to add content.
- **Trigger-rich**: mobs, objects, and rooms can run Lua via the DG-
  Scripts-derived trigger system. Triggers are shared (one trigger
  attaches to many entities via junction tables).
- **Persistent player state**: housing, account banks, account-wide
  mail, achievements, clans, federated identity (Discord/Google).
- **Quest depth**: phases > objectives, dialogue trees, branching
  via exclusive groups, multiple trigger sources (talk, level, item,
  room, skill, event, auto, manual).
- **Pre-release**: no live data to protect; willing to make breaking
  schema changes if the new shape is meaningfully better.

## What the schema does well

Patterns that genuinely serve the vision:

1. **Composite `(zoneId, id)` keys for world entities** — zones are
   independently authorable units; legacy IDs round-trip cleanly. A
   builder can edit zone 30 without coordinating with anyone editing
   zone 31. Most successful MUDs have this property; some lose it
   when they switch to surrogate keys and have to manage cross-zone
   ID collisions.
2. **Shared triggers via junction tables** — one trigger script can
   attach to N mobs / objects / rooms. Lets a builder write a
   `generic_shopkeeper_greet` once. This is a *win over CircleMUD*
   which inlined trigger references in each entity file.
3. **Effect system extensibility** — builders can compose new gameplay
   states by combining existing primitives (Effect rows + Lua hooks
   + junction-table attachments) without schema changes. The
   `effectType` discriminator + JSON parameter payload is the right
   shape for this kind of open content.
4. **Per-class spell circle progression as data**
   (`ClassAbilityCircles.minLevel`, `SpellSlotProgression`,
   `RaceSpellSlotBonus`) — caster pacing is a tuning table, not
   hardcoded math. Designers can rebalance without recompiling.
5. **Mob role classification** (`MobRole`: TRASH/NORMAL/ELITE/
   MINIBOSS/BOSS/RAID_BOSS) + per-class HP/AP curves — gives combat
   tuning a clean axis.
6. **Soft-delete on user-authored entities** (Mobs, Objects, Rooms,
   Characters, Shops, Zones, Users) — builders can recover from
   mistakes. Reference data (Effect, Liquid, Ability) doesn't have
   it, which is correct: those are catalog data.
7. **Junction-based item containment** (`CharacterItems.containerId`
   self-FK) — bags of holding, nested containers all work without
   schema changes.
8. **GMCP-ready structured player state** — Discord-integrated mail,
   federated identity, runtime achievement progress as JSON. Modern
   community features have first-class data homes.
9. **Audit trail** (`AuditLogs`, `ChangeLogs`, `ScriptErrorLog`) —
   debuggability is built in, not bolted on.
10. **Quest depth** — phases > objectives > dialogue, choice-group
    rewards, exclusive-group branching, multi-source triggers. The
    schema can express almost any quest you'd want to author.

## Where the schema fights the vision

Patterns that hurt builder/player/dev experience. I've grouped these
by severity — **structural** changes the schema shape, **stringly
typed** loses validation, **polish** is rough edges.

### Structural

**S1. Stringly-typed `Ability.abilityType` and `Effect.effectType`.**
Both are `String` with documented values (SPELL/SKILL/SONG/CHANT for
Ability; modify/damage/heal/status/room/.../etc. for Effect). The
runtime dispatches on them by string match. Should be enums — typos
in Muditor create invalid rows; Rust match arms can't be exhaustive-
checked. **Migration: cheap.** ⚠️ Grilling target.

**S2. `Triggers.commands String` — one big blob.** All trigger logic
is a single text column. No structure, no per-step validation, no
syntax-error column unless we add one (`syntaxError` exists; needs
to be populated by a checker). Modern editor UX (syntax highlight,
autocomplete) needs structure. **Tradeoff:** typing this means
defining a Lua AST → effectively building a structured editor.
Big lift. ⚠️ Grilling target — accept opaque or invest in structure?

**S3. `Objects.values Json` is silently polymorphic by `ObjectType`.**
A `PORTAL` object stores destination in `values`; a `LIGHT` stores
fuel; a `FOOD` stores nutrition; a `POTION` stores spell info. The
shape per ObjectType is undocumented. Builder editing an item in
Muditor has no schema hint about what to put in `values`. Either:
(a) document the shape per ObjectType in muditor's UI / a separate
table, (b) break out type-specific columns (e.g. `portalDestZoneId`,
`portalDestId`, `lightFuelRemaining`), or (c) keep JSONB but ship a
TypeScript Zod schema per ObjectType for client validation. ⚠️
Grilling target.

**S4. `Quests` trigger-type field + nine nullable trigger FKs.** The
`triggerType` enum (MOB/LEVEL/ITEM/ROOM/SKILL/EVENT/AUTO/MANUAL)
plus nine nullable trigger fields (triggerMobZoneId, triggerMobId,
triggerLevel, triggerItemZoneId, triggerItemId, triggerRoomZoneId,
triggerRoomId, triggerAbilityId, triggerEventId) is the same
polymorphism pattern as `ObjectEffects` but messier — no enforced
invariant that "exactly one set matches triggerType." A bad row
could have `triggerType=MOB` but only `triggerLevel` set, or have
multiple set. **Options:** check constraint, or a single
`triggerRef Json` field shaped per type, or table-per-trigger-type
+ union view. ⚠️ Grilling target.

**S5. Room flags vs `Sector` enum.** Room has 12 boolean columns
(`isPeaceful`, `allowsMagic`, `allowsRecall`, `allowsSummon`,
`allowsTeleport`, `isDeathTrap`, `isIndoors`, `isSoundproof`,
`isArena`, `isGuildhall`, `allowsMobs`, `allowsTracking`,
`allowsPortals`, `allowsScanning`) plus `entryRestriction` Lua plus
the `Sector` enum. A new flag means a schema migration. A
`roomFlags RoomFlag[]` enum array would scale better and clean up
the model. **Tradeoff:** named columns are discoverable in the
Prisma client (`room.allowsMagic`); flag arrays force a `.includes()`
check. ⚠️ Grilling target.

**S6. `Mobs` carries six attribute scores (STR/DEX/INT/WIS/CON/CHA,
default 13) AND eleven modern combat columns.** The attribute scores
were the only combat input in legacy CircleMUD; the new system uses
accuracy/attack_power/etc. Are mob attributes vestigial (drop them),
non-combat-only (keep for Cha checks vs shopkeepers, Int gates for
puzzles), or in-flight (planned use)? ⚠️ Grilling target.

### Stringly typed

**T1. `Characters.gender String @default("neutral")`** while
`Mobs.gender Gender @default(NEUTRAL)` is an enum. Same concept,
two representations. Drop the String, use the enum.

**T2. StatModifier `target` strings.** Already covered in the
cleanup plan — kept stringly-typed deliberately for extensibility.
Tradeoff is understood; flagged here for completeness.

**T3. Many Lua expressions stored as `String?`** —
`Quests.availabilityRequirement`, `Rooms.entryRestriction`,
`Mobs.aggressionFormula`, `Mobs.activityRestrictions`,
`ShopItems.visibilityRequirement` / `purchaseRequirement`,
`ShopMobs` same, `AbilitySavingThrow.dcFormula`,
`QuestObjectives.luaExpression`, `QuestRewards.condition`,
`AbilityRestrictions.customRequirementLua`. None are validated at
save time. A typo in Muditor produces a row that errors at first
execution (caught by `ScriptErrorLog`, but the player saw a glitch).
Either compile-check in Muditor on save, or run a sanity sweep on
import / startup. ⚠️ Grilling target.

### Polish

**P1. Singular vs plural table names.** `Mobs` / `Objects` / `Shops`
/ `Triggers` / `Zones` are plural; `Room` / `Effect` / `Ability` /
`Liquid` / `Clan` / `Achievement` are singular. Convention drift.
Picking one is cheap; either is fine. ⚠️ Grilling target.

**P2. `QuestDialogue` vs `DialogueTrees`.** Simple cases use the
inline `QuestDialogue` row on an objective; complex cases use a
`DialogueTrees` graph. Two systems doing similar work. Could be
unified (always-trees-of-one-node for simple cases). ⚠️ Grilling
target — keep or unify?

**P3. `ObjectAbilities` vs `ConsumableEffect`.** Both attach
gameplay effects to objects. `ObjectAbilities` is "object grants
ability when activated" (quaff potion → cast spell);
`ConsumableEffect` is "consuming the object applies an effect
directly." Conceptually overlapping; a new builder might not know
which to use for a healing potion. ⚠️ Grilling target.

**P4. `GameConfig` is stringly-typed (value String + valueType
String).** Admins can add new settings without schema migration —
real flexibility. But the runtime parses every config value at
read time. Tradeoff is healthy if read frequency is low.

## Comparison to industry patterns

What other game backends do, and where FieryMUD lands relative to
them. (Calibration only — not "they do X so we should." Just
pattern recognition.)

### Patterns we follow that are well-established

- **Zone-scoped composite keys** — every successful MUD lineage (CircleMUD, Smaug, ROM, Aardwolf, AzerothCore for WoW emulation) uses zone-scoped IDs for cross-content portability. We do this for world-prototype tables. Industry standard.
- **Junction-table trigger attachment** — modern MUD designs (and MMO emulators) moved away from inline trigger-id-in-file to junction tables for sharing. We do this. Win.
- **Catalog tables with `name` / `code` unique key** — universal for reference data (`Effect`, `Ability`, `Liquid`, `Achievement`, `Social`). We do this. Win.
- **JSON columns for ECS round-trip** — common in modern game backends with ECS persistence (Bevy/Specs-style serialization). The `Characters.{scriptVars, cooldowns, pets, ...}` cluster matches this pattern. Justified.
- **Soft-delete column on user-authored content** — Prisma/Rails convention; we use it appropriately (user content gets it, reference data doesn't).
- **Per-class progression as data tables** (`LevelDefinition`, `SpellSlotProgression`) — modern MMOs and indie games (Diablo, PoE) lean hard on this. We do too.
- **Auditing as a separate table** (`AuditLogs`, `ChangeLogs`, `ScriptErrorLog`) — universal in serious backends. We have it.

### Patterns where we deviate, defensibly

- **Lua hooks stored in the schema** (`Effect.{onApply, onTick, onRemove}`, `Triggers.commands`, Lua-string columns). Most games store scripts in files referenced by name. Storing them in the DB lets builders edit through Muditor without filesystem access — but the DB becomes a script store with all that implies (versioning, syntax validation, hot reload). Defensible for a builder-first MUD; needs syntax-checking on save to be safe (G7).
- **Effect Toolbox / parameter Effects** — Diablo III shipped affix templates with JSON-ish params; many MMOs hardcode per-effect rows. Our toolbox approach is builder-friendly but pushes validation to the editor.
- **Single-table inheritance for `Ability`** — defensible (many CRPGs unify spell/skill/etc.) but has known costs (nullable noise, single-table mutex). G-D1.

### Patterns where we're unusual

- **Mixed resistance representation** (JSONB for 4 owners, typed table for objects). Industry skews to one or the other consistently. Our mixed state is genuinely unusual; G-A1 is the most important grilling target as a result.
- **`AbilityRestrictions.requirements Json[]`** — a Postgres array of JSONs, not a JSON array. Most schemas would use either one JSONB column with an array inside, or a typed `AbilityRequirement` row table. Worth a look.
- **Wide polymorphic FK clusters** without CHECK constraints. Industry sometimes uses these (cheap to author, expensive to validate) but normally with at least app-level invariants. G-C2.

### What this implies

The schema follows the dominant patterns where they matter most
(zone keys, junction sharing, catalogs, soft delete, audit). It
deviates only where there's real builder ergonomics to gain. The
**mixed-state items** are the ones to fix — not the deviations
themselves.

## Builder workflow

Mental walkthrough: a builder wants to add **"Ring of Flame Aura"** —
a ring that grants +2 fire damage on melee swings and a passive
fire-resistance debuff to nearby enemies. What's the path?

1. Create the `Objects` row — type=ARMOR (no RING type? ⚠️
   investigate), wearFlags=[FINGER]. Set weight/cost/level. So far
   so good.
2. The +2 fire damage on swings — this needs to be an `Ability`
   linked via `ObjectAbilities` with a fire-element damage component.
   Or maybe a trigger on `ATTACK`? Builder has two paths and they
   look different. **This is a builder-workflow question, not a
   schema question.**
3. The fire-resistance debuff aura — this would be a passive `Effect`
   that applies to room occupants. Today `RoomEnvironmentalEffect`
   only attaches Effects to rooms statically. There's no "object
   emits a room-effect while worn" pattern. ⚠️ Grilling target —
   is "gear-grants-room-effect" a deliberate omission or a gap?
4. Wire it to the wearer via `ObjectEffects` (StatModifier or status
   attachment depending on what +2 fire means mechanically).

**Finding:** the schema *can* express this, but the builder needs to
make multiple authoring decisions (Ability vs Trigger; how to model
the aura) without strong guidance. Muditor UI needs to lead the
builder toward the canonical pattern per item type. The schema is
flexible enough; the question is whether Muditor's authoring UI
matches.

## JSONB landscape

35 JSONB columns across the schema. They're not uniformly justified
— grouped by use:

### Runtime-component round-trip — **KEEP JSONB**

These serialize live ECS components; the shape is owned by Rust
serde, the editor is hands-off, and 1:1 mapping is documented in
the column comments. Touching the schema would create ECS/DB drift.

- `Characters.{scriptVars, cooldowns, spellCooldowns, effectInstances, pets, ignoreList, trophyData, killTrackingData}`

### Authored knobs — **AMBIGUOUS** ⚠️

The writers are humans, not Rust serde. Today the JSON shape is
governed by the Effect Toolbox UI + Zod (probably). If you ever
want per-Effect typed validation in Postgres, you'd need per-Effect
tables. Worth grilling.

- `AbilityEffect.overrideParams`, `Effect.defaultParams`, `AbilityRestrictions.requirements` (`Json[]` — a Postgres array of JSONs, rare shape), `AbilitySavingThrow.onSaveAction`

### Resistance cluster — **TYPE OUT (with friction)** ⚠️

`Characters.resistances`, `Mobs.resistances`,
`CharacterClass.resistances`, `Races.resistances` all store
`{ "FIRE": 25, ... }`. But Objects already has a typed
`ObjectResistance(objectZoneId, objectId, element, value)` table.
**Same concept, mixed storage** — biggest red flag in the schema.
Three options: (a) extend ObjectResistance pattern to all four
owners; (b) one polymorphic `Resistances(ownerType, ownerId,
element, value)`; (c) revert ObjectResistance to JSON for symmetry.
Mixed state is the worst answer. ⚠️ Grilling target G-A1.

### Type-specific value bags — partial extraction, **KEEP rest**

`Objects.values` carried combat-critical fields until the typed-
column migration (`armorPct`, `weapon_*` extracted). What remains
is portal dest, fuel, food, spell, liquid — discriminated by
`Objects.type`. Clean shape would be per-type sidecar tables
(`ObjectPortal`, `ObjectFood`, etc.) on `(zoneId, id)`. That's 5–7
tables for rarely-read data. **KEEP for now but document as a
deliberate JSON-of-last-resort.** Already in grilling queue (G3).

### Per-instance custom data — **KEEP JSONB**

`CharacterItems.customValues`, `AccountItems.customData`,
`PlayerHouseItem.customValues`. Instance overrides where shape
varies by object type and is read only on examine. JSON is correct.

### Audit / log / free-form — **KEEP JSONB**

`AuditLogs.{oldValues, newValues}`, `ChangeLogs.changes`,
`ScriptErrorLog.contextInfo`, `EntityVariables.value`,
`Users.preferences`, `CharacterAchievement.progress`. Truly
polymorphic, write-mostly. Correct.

### Type-out candidates with stable shape — **TYPE OUT** ⚠️

- `Board.privileges` — 8 fixed privilege-rule shapes encoded as JSON array. Should be `BoardPrivilege { boardId, privilegeType BoardPrivilegeType, minLevel, allowedRanks }` with an 8-value enum. ⚠️ Grilling target G-B1.
- `CharacterQuests.variables` — labeled "Legacy compatibility" in its comment. The modern `EntityVariables` already does this job for mob/object/room. Extend it with `CHARACTER_QUEST` as an `EntityType`, or add `CharacterQuestVariables`. ⚠️ Grilling target G-B2.

## Polymorphism / row-shape patterns

The schema has six polymorphism patterns. They range from clean to
muddled:

| Pattern | Shape | Verdict |
|---|---|---|
| `ObjectEffects` / `MobDefaultEffects` / `RaceEffects` / `CharacterEffects` | Discriminator = `Effect.effectType` via FK; payload = `modifierData` JSON | **Healthy** — FK + payload contract via `Effect.defaultParams`. Same shape repeated in 4 tables. |
| `Quests` trigger family | `triggerType QuestTriggerType` discriminator + 10 nullable trigger columns | **Muddled** — no DB invariant pinning the right column to the discriminator. ⚠️ |
| `QuestObjectives` target family | `objectiveType QuestObjectiveType` discriminator + 4 nullable target FKs + Lua | **Muddled** — same critique. ⚠️ |
| `QuestRewards` | `rewardType QuestRewardType` discriminator + amount/objectFK/abilityId/quantity | **Muddled** — smaller surface, same issue. ⚠️ |
| `ConsumableEffect` | Two-arm: `liquidId` set XOR `(objectZoneId, objectId)` set | **Mostly clean** — has unique constraints on each arm but no DB-level XOR. |
| `Ability.abilityType String` | Spell / Skill / Song / Chant; nullable spell-only metadata block | **Single-table inheritance ambiguity** — see structural finding S1 and G-D1. |

The three Quest-related patterns share the same shape (discriminator
+ wide nullable FKs) and the same problem. Worth treating as a
single grilling target: do they all get a CHECK constraint, a sum-
type JSON, or a polymorphic sidecar table?

## Composite-key consistency

`(zoneId, id)` is the standard for **world-prototype** tables:
`Mobs`, `Objects`, `Room`, `Shops`, `Triggers`, `Quests`, plus the
`QuestPhases` / `QuestObjectives` composite extensions. All correct.

Everything else uses surrogate `Int @id @default(autoincrement())`,
appropriate for instance / join / config tables.

Borderline cases:

- **Global reference data** (`Liquid`, `Effect`, `Ability`, `Social`, `Achievement`, `Board`, `CombatMessage`, `Command`) — surrogate keys are fine; portability via the `name`/`code`/`alias` unique columns.
- **`PlayerHouseRoom`** has surrogate id + `(houseId, localIndex)` unique. Could be `@@id([houseId, localIndex])` but would cascade FK changes to `PlayerHouseExit`/`Item`. Cosmetic.
- **`DialogueNodes` / `DialogueResponses`** — surrogate keys, no portability story. If dialogue trees ever export between zones, `(treeId, localId)` would help. Pre-content, fine for now.

## Naming and conventions

The schema actively fights its own conventions. Three axes of drift:

### Plural vs singular table names

Random:
- **Plural**: Mobs, Objects, Races, Shops, Triggers, Zones, Users, Quests, QuestPhases, QuestObjectives, QuestRewards, QuestPrerequisites, CharacterQuests, CharacterQuestObjectives, MobTriggers, Liquids, AuditLogs, BanRecords, ChangeLogs, Characters, DialogueTrees, DialogueNodes, DialogueResponses, Events
- **Singular**: Room, Effect, Ability, Social, Liquid (via @@map), Clan, Achievement, Board, Report, CombatMessage, Command, PlayerHouse*, AccountMail, TellMessage

Some models use `@@map` to fix the DB-side name while leaving the
Prisma model name disagreeing — so e.g. `Quests` Prisma model maps
to `Quest` table; the codebase reads `prisma.quests` while the DB
shows `Quest`. This is the worst of both worlds. ⚠️ Grilling target
G-E1.

### @@map casing

Two camps:
- **PascalCase tables**: ToolboxCategory, Class, HelpEntry, AccountMail, Board, GameConfig, LevelDefinition, Command, SystemText, LoginMessage, CombatMessage, PositionData, PositionMessage, SystemMessage, RoomEnvironmentalEffect, RaceSpellSlotBonus, SpellSlotProgression, Quest*, Dialogue*
- **snake_case tables**: account_items, user_grants, entity_variables, script_error_log, tell_message, clan, clan_member, achievement, character_achievement, discord_links, google_links, discord_config, reports, player_house*

The snake_case set skews toward recent additions. PostgreSQL
conventions favor snake_case. ⚠️ Grilling target G-E2.

### `@map` omissions

Many columns lack `@map` even where siblings have it. Example:
`Characters.wealth` (no @map) sits next to `bankWealth` (`@map("bank_wealth")`).
Same pattern in Objects. The omitted columns happen to be already-
lowercase, so they survive — but it's fragile.

### Enum casing / Race enum overload

Enum value casing is fine (SCREAMING_SNAKE_CASE everywhere). But
`Race` mixes playable races (HUMAN, ELF, ...), monstrous races
(TROLL, OGRE, ...), dragon types, dragonborn, AND generic categories
(HUMANOID, ANIMAL, PLANT, OTHER) in one flat list. Mob defaults to
HUMANOID; Character defaults to HUMAN. **Worth splitting into
`PlayableRace` + `CreatureKind`** if the editor surfaces those
choices separately. ⚠️ Grilling target G-D3.

## Many-to-many junction patterns

Junctions are mostly consistent. Three observations:

### Trigger junctions are unifiable

`MobTriggers` / `ObjectTriggers` / `RoomTriggers` have **identical
shape**: `(entityZoneId, entityId, triggerZoneId, triggerId,
createdAt)` with `@@id` composite. Could collapse to
`EntityTriggers(entityType EntityType, zoneId, entityId,
triggerZoneId, triggerId)` — same trick `EntityVariables` already
plays. Trade-off: typed FKs (current) vs schema simplicity + easier
"all triggers on this entity" queries. ⚠️ Grilling target G-C1.

### Effect junctions have an asymmetry worth noting

`MobDefaultEffects`, `ObjectEffects`, `RaceEffects`,
`CharacterEffects` all carry `effectId` + `modifierData` + `strength`.
**`RoomEnvironmentalEffect` is just `(room, effect)` — no strength
or modifierData**. The asymmetry is suspicious: can a room not
modify the effect's parameters? If not, document why; if yes, add
the columns. ⚠️ Same grilling target G-C1.

### Ability junctions diverge intentionally

`ClassAbilities`, `ClassSkills`, `RaceAbilities`, `MobAbilities`,
`CharacterAbilities`, `ObjectAbilities` each carry different
surrounding context (circle vs minLevel vs category vs charges).
Unification would be hard. **But `ClassAbilities` vs `ClassSkills`
is suspicious** — both reference `Ability` (where `abilityType`
already distinguishes SPELL/SKILL). Why two parallel junction
tables? Probably historical. ⚠️ Grilling target G-D2.

## Surprising shapes (worth a second look)

These made the agent (or me) stop and think:

1. **`Shops` is column-rich**: `flags ShopFlag[]` + (dropped) `tradesWithFlags` + 5 separate `String[]` message arrays (`noSuchItemMessages`, `doNotBuyMessages`, `missingCashMessages`, `buyMessages`, `sellMessages`). A `ShopMessages` sidecar (parallel to `AbilityMessages`) would tidy this. ⚠️ Grilling target G-F1.

2. **`Triggers.commands` is one big String** of DG-script source, parsed at runtime. `ScriptType.MOB | OBJECT | WORLD` is a tag, not a structural distinction — a MOB trigger and a WORLD trigger have different available commands but live in the same column shape. Already in queue as G2.

3. **`Ability` single-table inheritance**: `abilityType String @default("SPELL")` with a Spell metadata block (sphere, damageType, pages, memorizationTime, questOnly, humanoidOnly) all-nullable next to it. Skills don't have a parallel metadata block (`SkillCategory` and `SkillType` enums exist but aren't on Ability). Textbook STI ambiguity. ⚠️ Grilling target G-D1.

4. **`Effect.appliedByPositions` + `PositionData.appliedEffects`** form a circular relation — positions apply effects; effects can be applied by positions. Works, but suggests Position is doing double duty as a pseudo-entity. Worth a clearer model if you ever add more position-driven effects.

5. **`MobResets.zoneId`** is a top-level column **and** `mobZoneId` + `roomZoneId` exist. All three usually equal in practice. The redundancy isn't constrained — cross-zone resets are technically possible but not validated. ⚠️ Grilling target G-F2.

6. **`Characters.id String`** while Mobs / Objects / Room use Int. Likely a UUID or legacy import slug. Worth understanding the reason. ⚠️ Grilling target G-F3.

7. **`CharacterClass` model maps to table `Class`** — clean rename via `@@map`, but the Prisma name disagrees, so the codebase reads `prisma.characterClass` everywhere while the DB shows `Class`. Inconsistent.

8. **`AbilityRestrictions.requirements Json[]`** — a Postgres array **of JSONs**, not a JSON array. Unusual choice. Each element presumably has a small schema (`{type, value}`); could be a typed `AbilityRequirement` table.

## Subsystem coherence checks

How well does each subsystem hold together internally? Walking
through:

### Combat — Mostly coherent

**Inputs:** Characters/Mobs combat columns (accuracy, attack_power,
spell_power, evasion, armor_rating, soak, hardness, ward_percent,
penetration_*, resistances). Modifiers come from `ObjectEffects`
rows via `apply_modify_delta`.

**Coherent:** d100 contest, modern saves enum, damage type axis (verb
flavor) separated from element type axis (resistance), per-class HP
curves via `LevelDefinition`.

**Friction:**
- Resistance representation inconsistency (JSON for 4 owners vs typed table for objects) — G-A1.
- Mob attribute scores (STR/DEX/...) may be vestigial — G6.
- Combat triggers (`ATTACK`/`DEFEND` TriggerFlag) live as side-channel logic; the trigger system and the combat pipeline interact via the runtime, not visible at schema level. Probably fine.

### Magic — Cluttered by single-table inheritance

**Inputs:** `Ability` (with `abilityType` discriminating
SPELL/SKILL/SONG/CHANT), `AbilityEffect` junction, `AbilityDamageComponent` for multi-element spells, `AbilitySavingThrow`, `AbilityTargeting`, `AbilityMessages`, `AbilityComponent` (material spell components), `AbilityRestrictions`.

**Coherent:** The 1:1 sidecars (`AbilityMessages`, `AbilityTargeting`, `AbilityRestrictions`) keep optional structured data off the main `Ability` row — that's the right shape. Per-class circle progression is data-driven via `ClassAbilityCircles` + `SpellSlotProgression` + `RaceSpellSlotBonus`. Damage-type composition via `AbilityDamageComponent` cleanly supports multi-element spells.

**Friction:**
- Single-table inheritance on `Ability` — spell-only fields (sphere, damageType, pages, memorizationTime) are nullable on every Skill/Song/Chant row. ⚠️ G-D1.
- `ClassAbilities` + `ClassSkills` parallel junctions referencing the same `Ability` table — looks historical. ⚠️ G-D2.

### Quest — Powerful but polymorphism-heavy

**Inputs:** `Quests`, `QuestPhases`, `QuestObjectives`, `QuestRewards`, `QuestPrerequisites`, `QuestDialogue`, `DialogueTrees` / `DialogueNodes` / `DialogueResponses`, `CharacterQuests`, `CharacterQuestObjectives`.

**Coherent:** Phases > objectives > rewards is the right hierarchy. Branching via `exclusiveGroup`. Prerequisites table for chains. Time limits + cooldowns. Solo vs party scope. Repeatable quests with completion count. This is a complete quest model.

**Friction:**
- Three polymorphism clusters (Quests trigger, QuestObjectives target, QuestRewards) all with the same wide-nullable shape. ⚠️ G-C2.
- Dialogue duality: `QuestDialogue` for simple, `DialogueTrees` for complex. ⚠️ G9.
- `CharacterQuests.variables` Json is legacy holdout; `EntityVariables` already does this. ⚠️ G-B2.

### Shop — Functional but column-heavy

**Inputs:** `Shops`, `ShopItems`, `ShopMobs`, `ShopAccepts`. Plus
ShopFlag enum and (post-cleanup) the three restriction arrays.

**Coherent:** Shops can sell items (ShopItems) AND mobs/mounts
(ShopMobs). Spawn chance + Lua visibility/purchase requirements
support gated economies. Trade restrictions (after cleanup) cover
alignment/class/race uniformly.

**Friction:**
- 5 separate `String[]` message columns on the Shops row. ⚠️ G-F1.
- Buy/sell paths don't yet enforce trade restrictions (queued in cleanup plan).

### Effects — Healthy core with junction sprawl

**Inputs:** `Effect`, `ToolboxCategory`, `AbilityEffect` (effects on abilities), 5 entity-effect junction tables.

**Coherent:** Effect catalog is rich (28 effectTypes today); `ToolboxCategory` groups them for the builder UI; junction tables share shape (with one asymmetry — RoomEnvironmentalEffect).

**Friction:**
- `effectType String` should be enum. ⚠️ G1.
- 5 junction tables with similar shape could potentially unify. ⚠️ G-C1.
- ObjectEffects' singleton-modify-Effect pattern is functional but quirky — every StatModifier is conceptually an Effect attachment to a sentinel row.

### Triggers — Compact but opaque

**Inputs:** `Triggers` (zone-scoped composite key, `attachType`, `flags`, big `commands String`), 3 junction tables, `ScriptErrorLog`, `EntityVariables`.

**Coherent:** Junction-based sharing is great. ScriptErrorLog gives debuggability. EntityVariables handles per-entity persistent state. needsReview + syntaxError columns are right.

**Friction:**
- `commands` is one opaque blob. ⚠️ G2.
- Three identical junction shapes (`MobTriggers`/`ObjectTriggers`/`RoomTriggers`). ⚠️ G-C1.

## Grilling queue

20 targets, organized by impact. Tier 1 = structural / lock-in,
resolve early before content authoring picks up. Tier 2 = meaningful
cleanup, schedule. Tier 3 = polish, do opportunistically.

### Tier 1 — Resolve early (structural / lock-in)

These shape how content is authored. Changing them later is a
content migration, not just a schema migration.

- **G-A1: Resistance representation.** 4 owners use JSONB, 1 owner (Objects) uses a typed table. Pick one: (a) extend `ObjectResistance` pattern to all four; (b) one polymorphic `Resistances(ownerType, ownerId, element, value)`; (c) revert ObjectResistance to JSON. Mixed state is the worst.
- **G-C2: Quest polymorphism cluster.** Three patterns share the same wide-nullable + discriminator shape (Quests trigger / QuestObjectives target / QuestRewards). Pick: (a) leave wide, enforce via app/Zod; (b) Postgres CHECK constraint per discriminator; (c) sum-type JSON; (d) polymorphic sidecar.
- **G-D1: `Ability` single-table inheritance.** SPELL/SKILL/SONG/CHANT in one table with nullable spell-only metadata. Pick: (a) keep + promote `abilityType` to enum (smallest change); (b) split into 4 tables sharing a common Ability row (classic CTI); (c) extract `SpellMetadata` sidecar (like AbilityMessages).
- **G1: Stringly-typed discriminators.** Promote `Ability.abilityType` and `Effect.effectType` to enums. Coupled with G-D1 — same migration.
- **G-E1: Plural-vs-singular naming.** ~half the models are singular, half plural; `@@map` sometimes makes Prisma and DB disagree. Pick: (a) Rails-style singular models + plural tables; (b) plural everywhere; (c) singular everywhere. **Cheapest right now** — the longer content authoring runs, the more downstream code references model names.
- **G-E2: @@map casing.** PascalCase vs snake_case tables — pick one (Postgres convention favors snake_case). Coupled with G-E1.

### Tier 2 — Meaningful cleanup

These improve the editor experience and reduce footguns. Worth a
scheduled migration.

- **G3: `Objects.values` polymorphic JSONB.** Document per-type, break out into typed sidecars, or ship Zod validators? Combat fields were already extracted; rest is rarely-read.
- **G-B1: `Board.privileges` JSONB.** 8 stable privilege rules currently as JSON array. Type out as `BoardPrivilege` rows?
- **G-B2: `CharacterQuests.variables` JSONB.** Legacy holdout. Fold into `EntityVariables` (add `CHARACTER_QUEST` to `EntityType`)?
- **G-C1: Unify effect/trigger junctions.** 3 trigger junctions + 5 effect junctions share shape. Collapse to `EntityTriggers` / `EntityEffects` keyed on `EntityType + zoneId + entityId`? Trade-off: typed FKs vs schema simplicity.
- **G-D2: `ClassAbilities` + `ClassSkills` parallel junctions.** Both reference `Ability`. Merge into one with an extra column, or keep parallel because the surrounding columns differ?
- **G-D3: `Race` enum overload.** Mixes playable / monstrous / dragon / generic categories. Split into `PlayableRace` + `CreatureKind`?
- **G2: `Triggers.commands` opaque blob.** Accept the blob, or invest in structured representation (would also enable Muditor syntax highlighting)?
- **G7: Lua-expression validation.** Validate the ~10 Lua-string columns on save (in Muditor) instead of at first execution.
- **G5: Room flag columns.** 12 booleans vs `RoomFlag[]` enum array — discoverability vs scalability tradeoff.

### Tier 3 — Polish / smaller wins

Targeted cleanups that don't change content shape.

- **G-F1: Shops 5 message columns → ShopMessages sidecar.** Mirror the `AbilityMessages` pattern.
- **G-F2: `MobResets.zoneId` redundancy.** Top-level zoneId duplicates mobZoneId/roomZoneId. Drop or constrain.
- **G-F3: `Characters.id String`** while other PKs are Int — why? Investigate and either document or align.
- **G6: Mob attribute scores (STR/DEX/INT/WIS/CON/CHA, default 13).** Drop if vestigial, keep if used for non-combat checks, wire to combat if planned.
- **G9: `QuestDialogue` + `DialogueTrees`.** Unify into always-trees, or keep the duality for simple cases?
- **G10: `ObjectAbilities` vs `ConsumableEffect`.** Document clear per-object-type guidance, or unify?
- **G11: "Object emits room-effect while worn"** — deliberate omission or gap? Comes up for aura-style items.

## Process

The next step is grilling rounds. Suggested order:

1. **Block 1 (~3–4 targets):** G-A1 + G1 + G-D1 + G-E1/E2 — these are the load-bearing structural decisions. Resolving them informs everything else.
2. **Block 2 (~3 targets):** G-C2 + G-C1 + G-B2 — polymorphism rationalization.
3. **Block 3 (~remaining Tier 2 + 3):** opportunistic, as design intent firms up.

Once Tier 1 is resolved, the architectural review converts into a
sequence of focused migrations — each one slotting into the existing
`migration-plan.md` cadence.
