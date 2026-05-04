# fierymud-rs Design Docs

These documents describe the **modern** systems for the Rust rewrite — not
the legacy CircleMUD/FieryMUD shape. The C++ port (`~/Code/mud/fierymud/`)
preserved legacy mechanics for parity; this repo is allowed to break with
that wherever a clean modern design serves the game better.

Each doc is a *proposal awaiting review*. The schema in
`muditor/packages/db/prisma/schema.prisma` is the source of truth once a
proposal is accepted and the Prisma migration lands; until then the
proposal lives here and the runtime uses whatever the schema currently
ships.

## Design philosophy

1. **Database is the source of truth.** Behavior comes from rows, not
   from constants in code. If a builder needs to change a value in
   Muditor, that value lives in a column.
2. **The runtime is a clean reflection of the data.** No name-based
   fallbacks, no hardcoded lookup lists, no per-callsite intent
   reconstruction. A `match` on data is fine; a `match` that re-encodes
   what the schema *should* have said is not.
3. **No legacy tropes for their own sake.** AC/THAC0/dice rolls were a
   tabletop residue from CircleMUD. We're free to use modern game-design
   primitives — accuracy/evasion, percent armor, typed resistances —
   that read better and produce more interesting content.
4. **JSON is for genuinely variable shapes.** Resistances per element
   are JSON because the key set varies. `{"num":N, "size":M, "bonus":B}`
   is not — that's three columns waiting to happen.
5. **Single concept, single column.** Wherever the schema has two
   shadow columns for the same thing (`raceType` text + `race` enum,
   `playerClass` text + `classId` FK) the legacy text version goes.

## Documents

| Doc | What it covers | Status |
|---|---|---|
| [combat.md](combat.md) | Accuracy/evasion swing resolution, damage pipeline, crit/variance | **locked** (review pass 2 — ward gating) |
| [effects.md](effects.md) | Per-flag Effect rows, `prevents: Action[]`, application/wearoff Lua | **locked** (review pass 2) |
| [abilities.md](abilities.md) | Stamina costs as columns, target scopes (room-only AOE), restrictions, magicality flag | **locked** (review pass 2 — magicality + OQ closure) |
| [damage-types.md](damage-types.md) | The `DamageType` enum, resistance application order, on-hit composition, mitigation engagement | **locked** (review pass 1) |
| [objects.md](objects.md) | Typed columns replacing `Object.values` JSON, weapon shape, on-hit ability wiring | **locked** (review pass 1) |
| [posture-and-lifestate.md](posture-and-lifestate.md) | Splitting voluntary posture from incapacitation | **locked** (review pass 1) |
| [schema-reconciliation.md](schema-reconciliation.md) | Dead duplicates and legacy columns to delete | **locked** (review pass 1) |

## How to review

For each doc:

1. The **Design intent** section is the part to push back on hardest.
   If the goal is wrong, everything downstream is wrong.
2. The **Schema** section is what goes into the Prisma migration. Edits
   here drive a Muditor schema update.
3. The **Runtime** section is what the Rust code does once the schema
   lands. Edits here are independent of the migration.
4. The **Open questions** at the end are the forks I want input on
   before writing the migration. Pick the answer you want and I'll lock
   it in.

## Reference: C++ docs

`~/Code/mud/fierymud/docs/` describes the **legacy** system as
implemented for the C++ port. Those docs are accurate descriptions of
what the legacy game does; they are not the design target for this
project. Where a new doc here lifts a concept from the C++ side, it
says so explicitly and explains the deviation.

## Divergence point — 2026-05-03

As of this date, the runtime intentionally diverges from the C++
port. Every locked design doc here describes a target that breaks
parity in the name of a cleaner modern shape. The key breaks:

- **Combat** — accuracy/evasion comparison replaces AC/THAC0/dice.
  See `combat.md`.
- **Effects** — per-flag catalog rows + 9 `AbilityEffect` kinds
  replace the legacy 142 EFF_* / APPLY_* flags and the C++ port's
  intermediate 33-kind consolidation. See `effects.md`.
- **Object data** — typed columns on `Objects` replace the
  legacy `Object.values` JSONB blob (kills the CircleMUD vnum
  residue). See `objects.md`.
- **Posture** — voluntary `Posture` enum split from
  incapacitation markers. See `posture-and-lifestate.md`.

**Operational note for the C++ side:** before changes that match
this divergence land here, snapshot or branch
`~/Code/mud/fierymud/` so the legacy parity work isn't lost.
Suggested:

```bash
cd ~/Code/mud/fierymud
git tag legacy-parity-snapshot
git checkout -b legacy-parity-frozen
```

After the tag, the C++ tree can either be retired or kept as a
reference implementation. The Rust runtime no longer aims to
reproduce its behavior — the modern docs in this directory are
the design target.
