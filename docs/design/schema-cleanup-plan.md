# Schema cleanup plan (May 2026)

Audit triggered by an external review that flagged dead enums, unused
columns, and Rust components with no readers. Cross-checked every
claim against live DB + Rust + fierylib code; user reviewed and
approved the items below.

This is a deferred plan — apply once the typed-column migration
(commit `ace14bd`) and combat-tuning sweeps have settled.

Live-DB verification (2026-05-15):

| Claim | Verified |
|---|---|
| `Mobs.move = 0` for every row | 2139 / 2139 rows |
| `Shops.tradesWithFlags` used | 0 / 79 shops |
| FIREWEAPON / MISSILE / WORN / WALL / PEN counts | 1 / 1 / 4 / 2 / 25 |
| Dead-apply `ObjectEffects` rows (legacy targets) | 1096 rows |
| `max_stamina` apply consumer | LIVE — writes to `Stamina.current/max` at `commands.rs:12370`; 190 ObjectEffects rows depend on it |
| `max_mana` apply consumer | DEAD — writes to `Mana` component; `Mana` loaded into `PromptCtx` at `commands.rs:5234` but `render_prompt` has no `%m`/`%M` token reader |

## Background

The audit followed the architectural rule recorded in
[[project-legacy-value-removal]]: the database stores ONLY modern
values; all legacy → modern conversion happens in fierylib at
import time. The runtime should have zero legacy alias paths.

The agent's original list flagged some real dead code but also
recommended dropping things that have legitimate intent. After
verification (every "dead" claim was checked against the live DB and
Rust grep), the decisions below diverge from the agent's list in one
notable place (PEN).

## Keep/drop criterion

The "modern values only" rule alone doesn't predict every decision —
some unwired plumbing is kept because we plan to wire it later.
Explicit criterion for future audits:

> Drop unwired plumbing **unless** there's a *recorded* design intent
> to wire it later. "Recorded" means a referenced doc, code comment,
> or parking-lot entry — not "I might want this someday." Items kept
> on intent must point to where that intent is recorded.

Items kept on intent in this pass:

| Item | Intent recorded at |
|---|---|
| `Perception` column + component | `info.rs` TODO sites (scan / search / spot-hidden / see-invisible) — wiring is queued ASAP |
| `ObjectType.PEN` (25 items) | Used today for writing notes / books; also a future hook for legacy spellbook-scribing if restored |

`ObjectType.WALL` was previously kept on intent ("for wall spells"),
but verification showed the four wall spells (`WALL_OF_FOG`,
`WALL_OF_ICE`, `WALL_OF_STONE`, `ILLUSORY_WALL`) use a **room-level
Effect** to seal a passage, not `ObjectType.WALL` items. No surviving
intent — moved to the drop list.

## What gets dropped (zero usage anywhere)

### Schema (muditor `packages/db/prisma/schema.prisma`)

| Item | Action | Verified |
|---|---|---|
| `Mobs.move` column | DROP | fierylib hardcodes 0 (`mud/types/mob.py:83`); `MovementPoints` Rust component has no reader |
| `ApplyType` enum | DROP entirely | Defined in schema but **not referenced by any column** in any model. No Rust or TS consumer. fierylib has its own separate Python `ApplyTypes` enum. |
| `Shops.tradesWithFlags` column | DROP | 0 rows have any value; agent recommended drop, user wants restructure (below) |
| `ShopTradesWith` enum | DROP entirely | Replaced by structured columns (below) |
| `TriggerFlag.AUTO` | DROP | 0 rows use it; schema comment already labeled "Legacy, may be unused" |
| `Direction.NONE` | DROP | No Rust references |
| `RaceAlign.UNKNOWN` | DROP | DB has only GOOD/EVIL; UNKNOWN is the unused default |
| `ObjectType.FIREWEAPON` | DROP value + delete the 1 item | "a flintlock pirate pistol" L100 god flavor |
| `ObjectType.MISSILE` | DROP value + delete the 1 item | "a silver bullet" L100 god flavor |
| `ObjectType.WORN` | DROP value | 4 items recategorize to ARMOR (below) |
| `ObjectType.WALL` | DROP value + delete the 2 items | Wall spells use a room-level Effect, not WALL-typed items. 2 god-tier flavor items in zone 125. |

### Schema additions (Shops trade restrictions restructure)

The legacy `SHOP_TRADES_WITH` field expressed alignment/class
restrictions on shopkeepers (NO_GOOD, NO_CLERIC, etc.). fierylib
parses it but never wrote it to the DB — value was dropped on the
floor. The schema enum was a confused mix of categories
(ALIGNMENT/RACE/CLASS) and specific values (TRADE_NOGOOD…), and
race-based restrictions weren't expressible at all (couldn't author
"this drow merchant won't trade with elves" without restructure).

Match the existing `Objects` restriction pattern:

```prisma
model Shops {
  // existing fields...
  restrictedAlignments Alignment[] @default([]) @map("restricted_alignments")
  restrictedClassIds   Int[]       @default([]) @map("restricted_class_ids")
  restrictedRaces      Race[]      @default([]) @map("restricted_races")
}
```

fierylib then maps legacy:
- `NO_GOOD` → `restrictedAlignments=[GOOD]` (etc.)
- `NO_CLERIC` → `restrictedClassIds=[<cleric.id>]` (etc.)
- `NO_MAGIC_USER` → `restrictedClassIds` containing all caster class ids
- New race restrictions become builder-authored content via Muditor UI

**Behavior delta of this pass:** Before — shop alignment/class/race
restrictions are not enforced (legacy `SHOP_TRADES_WITH` dropped on
the floor by the importer; 0/79 shops carry a value). After —
restrictions are *authored* in the DB (importer + Muditor UI populate
them) but still not enforced. Enforcement lands when
`shop_can_trade_with` is wired (see "Next code changes" below).

### Rust runtime (`fierymud-rs`)

| Item | Action | Notes |
|---|---|---|
| `apply_modify_delta` arms: `max_mana`, `max_movement`, `age`, `char_weight`, `char_height`, `composition`, `level`, `size` | DROP | Components they write into have no readers (verified by grep) **and** 0 ObjectEffects rows in live data for any of them. `max_movement` is a redundant alias for `max_stamina`; the rest have no schema/component/content infrastructure. **Note:** `focus` and `hit_regen` are NOT in this list (kept on intent — see below); legacy saves are remapped (see below); `hiddenness` is remapped to `concealment` (see below). |
| `apply_modify_delta` arms ADD: `concealment` | ADD | Mirror of the existing `perception` arm. Writes to a new `Concealment(i32)` component on the target. Fed by fierylib's hiddenness → concealment remap (4 rows). |
| `MovementPoints` component | DROP + insertion sites in `loader.rs`, `respawn.rs`, `admin_world.rs` | No tick consumer; per parking-lot.md "neither wander decrement nor regen tick is wired" |
| `SavingThrows` component | DROP + insertion site | Legacy AD&D saves; superseded by the modern `SaveType` (REFLEX / FORTITUDE / WILL). See [legacy-save migration](#legacy-save-migration-fierylib) below. |
| `RegenBonus` component | DROP + insertion site | No reader |
| **Full Mana removal** — `Mana` component, all three load sites (`commands.rs:4802` mstat, `:5234` PromptCtx, `:5292` GMCP vitals frame), `RegenBonus.mana` field (dies with RegenBonus), the `max_mana` apply arm, stale comments at `:4694` and `:10234` | DROP | Mana is a non-concept in this MUD — casting is gated by **spell circles** (`SpellSlotProgression` + `Cooldowns`), not a mana pool. **Behavior delta:** the GMCP vitals payload loses `mp` and `max_mp` fields; pre-release, so OK. |
| `ObjectType::Fireweapon` / `Missile` / `Worn` / `Wall` match arms (if any) | DROP | Once schema enum values are gone |

### fierylib

| Item | Action |
|---|---|
| `_LEGACY_AFFECT_MAP` entries: `MAX_MANA`, `MAX_MOVEMENT`, `AGE`, `CHAR_WEIGHT`, `CHAR_HEIGHT`, `COMPOSITION`, `LEVEL`, `SIZE` (in `object_importer.py`) | DROP rows |
| `_LEGACY_AFFECT_MAP` entry: `HIDDENNESS` → `concealment` | REMAP — 4 authored rows survive |
| `_LEGACY_AFFECT_MAP` entries: `SAVING_BREATH` → `saving_reflex`; `SAVING_PARA` / `SAVING_PETRI` → `saving_fortitude`; `SAVING_SPELL` / `SAVING_ROD` → `saving_will` | REMAP — 298 authored rows survive |
| `_LEGACY_AFFECT_MAP` entries: `FOCUS`, `HIT_REGEN` | KEEP — content + intent (710 + 82 rows) |
| `mob.move` parser line | DROP |
| Recategorize 4 `WORN` objects → `ARMOR` at import time | UPDATE |
| Drop FIREWEAPON / MISSILE / WALL items from import | UPDATE |
| Wire shop importer to populate `restrictedAlignments` / `restrictedClassIds` / `restrictedRaces` from legacy `SHOP_TRADES_WITH` | ADD |
| `mud/types/__init__.py::ApplyTypes` enum | TRIM to match modern targets only (incl. new `SAVING_REFLEX` / `SAVING_FORTITUDE` / `SAVING_WILL`) |

### Rust vocabulary renames

Align the Rust code with the canonical `StatModifier` term locked
into [Muditor's CONTEXT.md](../../../muditor/CONTEXT.md). All
mechanical — no behavior change.

| Current | Action | Where |
|---|---|---|
| `apply_modify_delta` (fn) | Rename → `apply_stat_modifier` | `commands.rs:12259` |
| `reverse_modify_delta` (fn) | **DELETE** — inline `apply_stat_modifier(target, stat, -delta.amount)` at the one callsite with a comment | Definition `commands.rs:12478`; caller `effects.rs:228` |
| `ModifyDelta` (component) | Rename → `StatModifierRecord` | Definition `components.rs:1798`; ~14 use sites across `effects.rs`, `login.rs`, `commands.rs`, `commands/info.rs`, `mud-world/src/lib.rs` |

`fierylib`'s `_LEGACY_AFFECT_MAP` keeps its name — it's already
labeled legacy and will be removed wholesale once the importer's job
is done.

### Data migration (one-shot SQL)

```sql
-- Re-categorize 4 WORN items as ARMOR
UPDATE "Objects" SET type = 'ARMOR' WHERE type = 'WORN';

-- Delete god-tier flavor items for dropped types
DELETE FROM "Objects" WHERE type IN ('FIREWEAPON', 'MISSILE', 'WALL');

-- Drop dead ObjectEffects rows (write-only targets that nothing reads).
-- Expected: ~2 rows deleted post-reimport (just max_mana). The 298 legacy
-- save rows and 4 hiddenness rows are REMAPPED by fierylib to modern
-- targets, not deleted — they re-import under saving_reflex / saving_fortitude
-- / saving_will / concealment.
-- EXCLUDED — live consumers preserved:
--   * max_stamina (190 rows): writes to Stamina.current/max
--   * focus (710 rows): kept on intent
--   * hit_regen (82 rows): kept on intent
-- Run this only AFTER a full fierylib reset/reimport so the legacy
-- rows have been migrated; otherwise this deletes content the importer
-- hasn't yet re-emitted under modern names.
DELETE FROM "ObjectEffects"
WHERE modifier_data->>'target' IN (
  'max_mana','max_movement',
  'age','char_weight','char_height','composition','level','size'
);
```

## What's intentionally KEPT (against the agent's recommendation)

### `ObjectType.PEN` (25 rows)

Agent recommended dropping. **Keep**:
- 25 content-authored items (feather quills, styluses)
- Tied to the legacy spellbook-scribing flow (hold pen + spellbook +
  trainer to scribe). Modern spell system bypasses scribing
  (`study <spell>` works anywhere with no items), but user wants
  the option to restore legacy scribing later.
- General writing requires a pen regardless of scribing.

### `Perception` column + component

Agent recommended keep. **Confirmed kept**:
- Plumbing is real: column on Mobs + Characters, flows into Rust
  component, displayed in `mstat`, gear can grant +perception via
  `apply_modify_delta` arm.
- **Not currently consumed** by any gameplay check — `info.rs` has
  four TODO sites: `scan` (passive), `search` (active), spotting
  hidden actors, see-invisible. All marked "no perception roll yet".
- 92 `ObjectEffects` rows depend on `target='perception'`.
- User intent: passive perception for scan/auto-spot, active
  perception for search. Wiring is queued — see "Next code changes".

### `Focus` and `hit_regen` (added by mid-flight audit)

The original drop list flagged these as dead because their
components have no runtime readers. Re-checked against the
[keep/drop criterion](#keepdrop-criterion):

| Test | Focus | hit_regen |
|---|---|---|
| Plumbing exists | ✓ `Focus` component + apply arm + `Races.focus_bonus` column (default 100) | ✓ `RegenBonus.hp` field + apply arm |
| Content depends on it | ✓ **710** `ObjectEffects` rows (largest target in DB) | ✓ 82 `ObjectEffects` rows |
| Recorded design intent | ✓ `combat-rebalance.md:368` ("focus stored on player… follow-up wiring") + component docstring naming "spell-slot regen rate modifier" | ✓ `combat-rebalance.md:368` |

Both satisfy the keep criterion. **Action:** keep their apply arms,
keep the `Focus` component, keep the `RegenBonus` component (was
slated for drop — reversed), exclude `focus` and `hit_regen` from
the SQL DELETE, add wiring to "Next code changes".

**Note on doc staleness:** `combat-rebalance.md:368` also claims mana
and saves are "wired into combat." Verification (2026-05-15) shows
neither is — the `Mana` component has no combat-tier reader and
neither `SaveType` nor `SavingThrows` is consumed in `combat.rs` or
`effects.rs`. Update that doc when this cleanup ships.

### Legacy-save migration (fierylib)

The legacy AD&D saves (`saving_para`, `saving_rod`, `saving_petri`,
`saving_breath`, `saving_spell`) total **298 authored
`ObjectEffects` rows** (`saving_spell` 184 + `saving_breath` 70 +
`saving_para` 38 + `saving_petri` 3 + `saving_rod` 3). Schema has
explicitly replaced them with the modern `SaveType` enum
(REFLEX / FORTITUDE / WILL) per its inline comments.

Mirror the [shop trade restrictions restructure](#schema-additions-shops-trade-restrictions-restructure)
pattern: fierylib maps legacy → modern at import time so 298 rows of
authored save-bonus gear survive the cleanup. Mapping per the
SaveType enum's own comments:

| Legacy target | Modern target | Source |
|---|---|---|
| `saving_breath` | `saving_reflex` | "REFLEX… replaces legacy BREATH" |
| `saving_para` | `saving_fortitude` | "FORTITUDE… replaces legacy POISON, PARALYSIS, PETRIFICATION" |
| `saving_petri` | `saving_fortitude` | same |
| `saving_spell` | `saving_will` | "WILL… replaces legacy SPELL, ROD, WAND" |
| `saving_rod` | `saving_will` | same |

This means:
- Add `saving_reflex` / `saving_fortitude` / `saving_will` arms to
  `apply_modify_delta` (writing to a new `Saves` component shaped on
  `SaveType`); these are the modern apply-targets.
- The SQL DELETE in this cleanup still removes the 298 legacy rows —
  but only after fierylib has re-imported them as modern targets in
  a full reset.

## Sequencing

1. Schema: `db push` the column drops + additions (one Prisma file diff)
2. Regenerate Python prisma client (`prisma generate --generator py`)
3. fierylib: importer updates + ApplyTypes enum trim
4. Full reset + reimport so DB reflects new shape
5. Rust: drop apply_modify_delta arms + dead components + insertion sites
6. Rust: vocabulary renames (`apply_modify_delta` → `apply_stat_modifier`; delete `reverse_modify_delta`; `ModifyDelta` → `StatModifierRecord`)
7. SQL migration for the few items that didn't come through the importer rewrite (none expected after a full reset)
8. Verify: `cargo test`, score sheet shows correct stats, in-game shop trade with restricted class is refused (when runtime check is wired)

## Next code changes (queued — land soon after this cleanup ships)

Both of these are *data is in place, runtime check is missing*
situations created by this cleanup. They're called out separately
from open follow-ups because the cleanup deliberately stops short of
enforcement and the enforcement is the obvious next step.

### 1. Wire `shop_can_trade_with(shop, char)`

- **Entry points:** buy / sell / list command paths in `mud-server`
- **Inputs:** `Shops.restrictedAlignments / restrictedClassIds / restrictedRaces` (populated by this cleanup)
- **Semantics:** if any of the three lists contains a match for the character, the shopkeeper refuses to trade — emit the existing `do_not_buy_messages` / `no_such_item_messages` if non-empty, else a default refusal
- **Test:** evil-aligned character can't buy from a `restrictedAlignments=[EVIL]` shop; same shape for class / race

### 2. Wire perception ↔ concealment consumers

Perception and concealment are paired axes — detect vs hide. Wire
both together; one is meaningless without the other.

- **Entry points:** four TODO sites in `info.rs` — `scan` (passive), `search` (active), spot-hidden-actors, see-invisible
- **Inputs:** `Perception` on the looker (gear sum + `Characters/Mobs.perception` column); `Concealment` on the target (gear sum + `Characters/Mobs.concealment` column). Both have the same plumbing shape.
- **Semantics:** roll perception vs concealment; success reveals, failure stays blind. Passive checks run on room entry / look; active runs on the `search` command.
- **Test:** character with high `Perception` spots a hidden mob that a baseline-perception character can't; conversely, a high-`Concealment` character isn't spotted by a baseline-perception looker.

### 3. Wire `Focus` consumer

- **Entry points:** the memorize / spell-slot regen tick (wherever `SpellSlots.in_flight` decrements)
- **Inputs:** `Focus` component on the caster (gear sum) + `Races.focus_bonus` baseline (default 100, applied as percentage)
- **Semantics:** Focus scales how fast spell-slot cooldowns refill. Positive values speed regen; negative slow it. Locked-in by 710 ObjectEffects rows of authored content and by every race's focus_bonus value.
- **Test:** a player with +50 focus regens a spent spell slot measurably faster than a baseline player.

### 4. Wire `hit_regen` (and `stamina_regen`) consumers

- **Entry points:** the per-tick HP / stamina regen rate calculation
- **Inputs:** `RegenBonus { hp, stamina }` on the entity (sum of gear contributions)
- **Semantics:** flat additions to the per-tick gain on top of the base regen scaling. Negative values valid (cursed gear that slows regen).
- **Test:** a player wearing +5 hit_regen gear gains HP per tick faster than baseline.

### 5. Wire modern saves (REFLEX / FORTITUDE / WILL)

- **Entry points:** `effects.rs` when applying a saving-throw-eligible effect (`AbilitySavingThrow.saveType` rows already in schema)
- **Inputs:** new `Saves { reflex, fortitude, will }` component on the target (replaces dropped `SavingThrows`); fed by `saving_reflex` / `saving_fortitude` / `saving_will` apply arms (added by this cleanup)
- **Semantics:** roll target's save score vs DC formula on the ability; on success apply `AbilitySavingThrow.onSaveAction` (NEGATE / HALF / etc.)
- **Test:** a high-Reflex character resists an AOE damage spell while a low-Reflex character takes full damage. **Required to make the 298 migrated save-bonus gear rows do anything.**

## Open follow-ups (NOT part of this cleanup, not next either)

1. Restore legacy spellbook scribing flow (pen + spellbook + trainer) — design call deferred per user
2. T7 unarmed-character investigation (separate audit task)
