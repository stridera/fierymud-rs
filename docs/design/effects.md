# Effects

**Status:** locked except where noted (review pass 1, 2026-05-03).

## Design intent

Today the `Effect` table has a small number of generic rows
(`status`, `damage`, `heal`, …) and abilities specialize them via an
`override_params.flag` JSON field. The runtime loads `EffectInstance`
rows whose `kind` points at the parent generic row, and the per-flag
intent (web vs paralysis vs hold) is reconstructed from the JSON.

This proposal **flips it**: the `Effect` table holds one row per
distinct status / mechanic — `web`, `silence`, `paralysis`, `bleed`,
`berserk`, `poison`, `bless`, etc. — and ability rows point at the
specific Effect they trigger. The runtime reads the row directly. No
JSON override-flag dance, no "find the catalog row whose name matches
the override" fallback.

The user-reported "web doesn't immobilize" bug is the canonical
symptom of the old shape: there's no per-flag prevent flag because the
parent row is too generic.

## Schema

### Effect

| Column | Type | Notes |
|---|---|---|
| `id` | Int | PK |
| `name` | String @unique | `"web"`, `"silence"`, `"bleed"`, `"strength_buff"` … |
| `display_name` | String | "webbed", "silenced", "bleeding" — what shows in `effects` listing |
| `description` | String? | Builder-facing explainer |
| `effect_type` | EffectType | `STATUS`, `DAMAGE`, `HEAL`, `MODIFY`, `CLEANSE`, `KNOCKDOWN`, `REDIRECT`, `STOP_COMBAT`, `DISPEL`, `STUN` |
| `category` | EffectCategory | `BUFF`, `DEBUFF`, `NEUTRAL`, `ENVIRONMENTAL` |
| `tags` | EffectTag[] | typed enum array — see below |
| `prevents` | Action[] | typed enum array — see below |
| `default_duration_secs` | Int? | When the spawning ability didn't override |
| `default_strength` | Int? | Same |
| `is_dispellable` | Bool | False for environmentals, weather-driven status, etc. Default true. |
| `is_visible` | Bool | False = hidden from `effects` listing. Default **true** (video-game convention — players see their buffs/debuffs). Opt-out for genuinely-narrative cases like cursed-item surprises. Stealth/Frozen/Ghost are not EffectInstance rows; they're separate ECS markers, so they're naturally absent from the listing. |
| `on_apply_lua` | String? | Lua source — runs once when the instance lands |
| `on_tick_lua` | String? | Lua source — runs every effect tick |
| `on_remove_lua` | String? | Lua source — runs when instance expires/is dispelled |

### `Action` enum (the new prevents shape)

```
ATTACK_INITIATE   # cmd_attack refuses (Calm Animal, Pacify)
ATTACK_CONTINUE   # combat-tick swing-snapshot skips this entity
CASTING           # spells refused
ABILITY_USE       # all skill/spell/song invocation refused (bigger hammer)
MOVEMENT          # cmd_move refused (web, snare, root, paralysis)
SPEAKING          # tell/say/gossip/ctell refused (silence variants)
TARGET_CHANGE     # locked onto current target (taunt, hold)
ITEM_USE          # quaff/recite/wave/tap/touch refused
LOOK              # blind
```

A row's `prevents` is the union of categories that effect blocks.

| Effect | prevents |
|---|---|
| `web` | `[MOVEMENT]` |
| `silence` | `[CASTING]` |
| `paralysis` | `[ATTACK_INITIATE, ATTACK_CONTINUE, MOVEMENT, ABILITY_USE]` |
| `hold_person` | `[ATTACK_INITIATE, ATTACK_CONTINUE, MOVEMENT, ABILITY_USE]` |
| `calm_animal` | `[ATTACK_INITIATE]` (defends if attacked, won't pick fights) |
| `pacify` | `[ATTACK_INITIATE]` |
| `taunt` | `[TARGET_CHANGE]` (locked on the taunter) |
| `blind` | `[LOOK]` |
| `entangle` | `[MOVEMENT]` |

The split between `ATTACK_INITIATE` and `ATTACK_CONTINUE` lets
content authors express "you can defend yourself but won't pick a
fight" cleanly. Most hold-style debuffs use both. Most peace effects
use only INITIATE.

#### Level-gating is orthogonal to prevent flags

A common content pattern is "this effect only lands on weaker
targets" (Calm Animal only affects mobs ≤ 3 levels below the caster).
That's **not** a prevent flag — it's a *target filter* on the
spawning ability. Implement via the existing `AbilityRestrictions`
evaluator with a new rule type:

```yaml
restrictions:
  - type: target_level_relative
    operator: <=
    value: caster.level - 3
    message: "{target.name} is too strong for {actor.you} to calm."
```

Effect rows themselves never carry "only for weak targets" — once
the effect lands, the prevent flags apply uniformly regardless of
who's wearing it. Two layers, both data-driven.

See [abilities.md](abilities.md) for the full restriction rule list.

### EffectInstance (runtime ECS, also persisted)

| Field | Notes |
|---|---|
| `effect_id` | FK to `Effect.id` (renamed from `kind` to make it clear) |
| `applied_to` | character / mob entity ref |
| `strength` | Per-instance numeric (heal amount, modifier delta, …) |
| `expires_at` | Or remaining secs; permanent if null |
| `source` | `EffectSource` enum: Spell, Skill, Item, Environment, Racial, Admin |
| `source_ability_id` | FK to `Ability.id` when source = Spell or Skill; otherwise null. Used to fetch the wearoff message via `AbilityMessages`. |
| `params` | Json — per-instance kv (modify-target name, redirect-pct, etc.) |

`EffectInstance.kind` is renamed `effect_id` to drop the legacy term.

We **do not** merge `source` + `source_ability_id` into a single
tagged-union column — Postgres / Prisma don't support tagged unions
cleanly, and the cost of CHECK constraints to enforce "exactly one
of these source-specific FKs is non-null" outweighs the Rust
ergonomics win. If we ever need richer per-source provenance
(`source_object_zone` / `source_object_id` for "which scroll did
this come from"), they go on as additional nullable columns.

#### Multi-effect abilities

An ability with multiple effects spawns multiple EffectInstances,
all carrying the same `source_ability_id`. A spell that does
"damage + bleed + slow" produces three rows on the target:

| effect_id | source | source_ability_id |
|---|---|---|
| `<damage>` | Spell | 42 |
| `<bleed>` | Spell | 42 |
| `<slow>` | Spell | 42 |

Each is independently dispellable / cleansable; the wearoff message
lookup uses `source_ability_id` to find the right
`AbilityMessages.wearoff_*` template.

### AbilityEffect

Restructured around a **kind discriminator** so authoring is
ability-centric. The C++ port (see
`~/Code/mud/fierymud/docs/game-systems/EFFECTS_REVIEW.md`)
landed on 33 parameterized kinds; we collapse to **9** because our
Lua-in-DB hooks let the Effect catalog carry per-status behavior
that C++ had to express as separate kinds.

```
AbilityEffectKind enum:
  DAMAGE          # weapon-type / spell damage; supports multi-element splits via multiple rows
  HEAL            # restore HP / Stamina / Mana
  MODIFY          # bump a typed stat for a duration; references a catalog row
  STATUS          # apply a persistent flag; references a catalog row
  CLEANSE         # remove EffectInstances by tag filter
  DISPEL          # filtered effect-removal (powered)
  REVEAL          # one-shot reveal (identify, locate, faerie fire)
  KNOCKDOWN       # instant posture-change to SITTING/RESTING (with optional follow-up Prone STATUS)
  CHAIN_DAMAGE    # bouncing-target damage with attenuation (chain lightning shape)
```

`AbilityEffect` carries `effect_kind` + per-kind columns. Most rows
populate 1–4 columns; the runtime branches on `effect_kind` and
reads the relevant set:

| Column | Populated when | Notes |
|---|---|---|
| `damage_amount` | DAMAGE, CHAIN_DAMAGE | formula string |
| `damage_type` | DAMAGE, CHAIN_DAMAGE | `DamageType` enum |
| `damage_pct` | DAMAGE | for multi-element splits; default 100 |
| `chain_max_jumps` | CHAIN_DAMAGE | how many hops through the room |
| `chain_attenuation` | CHAIN_DAMAGE | multiplier per jump (0.0–1.0) |
| `heal_amount` | HEAL | formula |
| `heal_resource` | HEAL | `HealResource` enum: HEALTH, STAMINA, MANA |
| `status_effect_id` | STATUS, MODIFY | FK → `Effect` catalog row |
| `override_params` | STATUS, MODIFY | JSON — duration / strength / per-instance kvs |
| `cleanse_tags` | CLEANSE | `EffectTag[]` to clear |
| `dispel_filter` | DISPEL | `EffectTag` to match |
| `dispel_scope` | DISPEL | `DispelScope` enum: FIRST, ALL |
| `dispel_power` | DISPEL | formula |
| `reveal_kind` | REVEAL | `RevealKind` enum: IDENTIFY_ITEM, LOCATE_OBJECT, LOCATE_PERSON, DETECT_INVIS, etc. |
| `knockdown_to` | KNOCKDOWN | `Posture` enum: SITTING, RESTING |

### What about teleport / summon / create / interrupt / etc.?

The existing fierylib catalog has world-mutating "instant" kinds
(`teleport`, `summon`, `create`, `extract`, `interrupt`, `move`,
`dismount`, `stop_combat`, `enchant`, `room`, `portal`, `resurrect`).
These don't get their own AbilityEffect kinds in our 9. Instead,
each becomes a **STATUS catalog row whose `on_apply_lua` performs the
action and self-removes:**

```lua
-- "Word of Recall" Effect catalog row, on_apply_lua:
self:teleport_to(self.recall_room)
effect:remove()
```

```lua
-- "Animate Dead" Effect catalog row, on_apply_lua:
self.room:spawn_mobile(15, 23)        -- skeleton proto
effect:remove()
```

```lua
-- "Word of Stop" Effect catalog row, on_apply_lua (a stop_combat row):
self:stop_combat()
effect:remove()
```

This keeps the kind enum small (9 instead of 33) and pushes the
"what does it actually do" into Lua where content authors can
iterate without schema changes. Trade-off: one indirection at runtime
(spawn EffectInstance → run Lua → despawn) vs C++'s direct
function call. The cost is a few microseconds and worth it for the
authoring win.

## Runtime

`effect_prevents(world, target, action)` is now:

```rust
pub(crate) fn effect_prevents(world: &mut World, target: Entity, action: Action) -> bool {
    let active_ids: Vec<i32> = collect_effect_ids_on(target);
    let catalog = world.resource::<EffectCatalog>();
    active_ids.iter().any(|id| {
        catalog.by_id.get(id).is_some_and(|def| def.prevents.contains(&action))
    })
}
```

That's it. No name-match, no fallback. If the action is in the row's
`prevents` array, it's blocked.

Removing an active effect named `"web"` becomes "find every
`EffectInstance` whose `effect_id` resolves to a row with
`name = 'web'`". For the cleanse / dispel paths, runtime queries by
`Effect.tags` or `Effect.name` against the catalog — same pattern, but
now the data has the answers.

## Lua hook contract

`on_apply_lua` / `on_tick_lua` / `on_remove_lua` run inside the LuaHost
with `self` bound to the affected entity and `effect` bound to a
`LuaEffectInstance` userdata exposing:

- `effect.name` / `effect.strength` / `effect.remaining_secs`
- `effect.source` — string
- `effect.params[<key>]` — read of the JSON
- `effect:remove()` — early dispel from inside the hook

The runtime already has these wired (see SUGGESTIONS 2026-05-01
fourth wake); we keep that shape.

## Stacking rules

The default rule for any non-MODIFY effect is **strongest-or-equal
wins**:

| Comparison | Outcome |
|---|---|
| New `strength` > existing `strength` | Replace: new strength + new duration |
| New `strength` == existing `strength` | Refresh duration |
| New `strength` < existing `strength` | **No-op** — application silently fails. The target keeps the stronger version; if they want the weaker one, they self-dispel first. |

Type-specific exceptions:

| Category | Rule |
|---|---|
| MODIFY (e.g. +str buff) | Stack **additively** — each application is its own EffectInstance + `ModifyDelta` companion record. Different from status effects because two clerics each casting their own +str on the same target should compound, not overwrite. |
| ENVIRONMENTAL (room aura) | Re-applied on each entry; expires on exit or when the room aura is removed. Stacking comparison N/A. |
| DAMAGE / HEAL / KNOCKDOWN (instant-effect types) | Not really stackable — they apply once and the EffectInstance immediately resolves. The strength comparison doesn't run. |

The same logic applies to DEBUFFs from the *target's* perspective: a
worse debuff (higher pain strength) overrides a milder one; refresh on
equal; weaker silently fails. Players take the worst of what's been
hit with; can dispel to clear.

## Migration plan

The starting point is `fierylib/data/effects.json` (28 polymorphic
"kind" entries with parameter schemas) and
`fierylib/data/abilities.json` (~370 abilities with `effects[]`
arrays referencing those kinds). The new shape splits:

- **Kinds** become a Rust enum (`AbilityEffectKind`) baked into the
  runtime — no longer data.
- **Status flags** (currently the polymorphic `status.flag` enum
  with ~35 values) become individual rows in the `Effect` catalog
  table, each with its own prevent flags, tags, Lua hooks, default
  duration.
- **World-mutating actions** (currently kinds like `teleport`,
  `summon`, `create`) become Effect catalog rows with `on_apply_lua`
  that performs the action and self-removes.
- **Abilities** keep their `effects[]` arrays but each entry shifts
  from `{effect: <kind>, params: {...}}` to `{kind: <enum>, …per-kind columns…}`.

### Step 1 — reshape `fierylib/data/effects.json`

The existing 28 entries map as follows:

| Existing entry | New shape |
|---|---|
| `damage` | Removed from effects.json — kind is now an enum |
| `heal` | Removed — kind |
| `modify` | Removed — kind. The `target` enum (str / dex / con / acc / eva / ap / ward / max_hp / regen_hp etc.) survives as `ModifyTarget` enum on the catalog row. |
| `status` (polymorphic w/ `flag` enum) | Expand into ~35 individual rows, one per flag — bless, sanctuary, fly, haste, paralyzed, sleeping, charmed, feared, confused, silenced, slowed, webbed, blinded, hidden, invisible, detect_*, infravision, ultravision, resistance, vulnerability, reflect, elemental_hands, lifesteal, taunted, poisoned, diseased, cursed, glowing, empowered, meditating, aware, berserk, featherfall, waterwalk, waterbreath. Each row carries its own prevent flags + tags + Lua hooks. |
| `cleanse` | Removed — kind |
| `dispel` | Removed — kind |
| `reveal`, `inspect` | Removed — kind (REVEAL with `reveal_kind` enum covers both) |
| `knockdown` | Removed — kind |
| `teleport`, `summon`, `create`, `resurrect`, `extract`, `interrupt`, `move`, `dismount`, `stop_combat`, `enchant`, `room`, `portal`, `conceal_item` | Each becomes a **STATUS catalog row** with `on_apply_lua` that does the work. Naming: `teleport_pending`, `summon_pending`, `create_food`, `resurrection_blessing`, `room_aura_<name>`, etc. |
| `globe`, `transform`, `intercept`, `redirect`, `drag`, `stun` | STATUS catalog rows — these are persistent statuses, not instants |

Net result: effects.json shrinks the kind list to zero, expands the
catalog to ~50–60 rows.

### Step 2 — runtime + schema migration

1. Add `EffectKind` Rust enum (compile-time).
2. Drop `Effect.preventsSpeaking` / `preventsCasting` / `preventsMovement`
   once the new `prevents Action[]` column is populated.
3. Add the typed columns on `AbilityEffect` (per-kind, mostly nullable).
4. fierylib seeder reshape: rename `effects.json` → `effect_catalog.json`
   to reflect that it's now a catalog of *statuses + on-apply actions*,
   not a kind dictionary. The existing damage/heal/modify entries are
   deleted entirely; new per-flag rows added.
5. Rename `EffectInstance.kind` → `effect_id` everywhere (search-replace).

### Step 3 — re-import abilities.json

Each `ability.effects[]` entry transforms based on its current
`effect` kind:

```yaml
# OLD
{ effect: "damage", params: { type: "fire", amount: "level + 6d6" } }

# NEW
{ kind: DAMAGE, damage_type: FIRE, damage_amount: "level + 6d6", damage_pct: 100 }
```

```yaml
# OLD
{ effect: "status", params: { flag: "bless", duration: "skill / 4" } }

# NEW
{ kind: STATUS, status_effect_id: <bless catalog row id>,
  override_params: { duration: "skill / 4" } }
```

```yaml
# OLD
{ effect: "modify", params: { target: "str", amount: 4, duration: "level * 2" } }

# NEW
{ kind: MODIFY, status_effect_id: <strength_buff catalog row id>,
  override_params: { strength: 4, duration: "level * 2" } }
```

```yaml
# OLD
{ effect: "teleport", params: { destination: "home" } }

# NEW
{ kind: STATUS, status_effect_id: <word_of_recall catalog row>,
  override_params: { destination: "home" } }
# (Catalog row's on_apply_lua reads override params and teleports.)
```

A fierylib migration script does the bulk transformation; manual
review handles edge cases (rare per-ability oddities).

### Step 4 — verify

- Spot-check 10 abilities across kinds (damage spell, buff, debuff,
  heal, summon, teleport, room aura).
- Smoke test: cast each, verify the Effect lands, verify the prevent
  flags fire when expected, verify wearoff messages emit.
- Diff the live `Ability` row count: should be unchanged.
- Diff the live `AbilityEffect` row count: should be roughly
  preserved (some abilities will gain rows when multi-element damage
  splits into multiple AbilityEffects).

## Decisions locked (review pass 2, 2026-05-03)

| Question | Locked |
|---|---|
| Number of AbilityEffect kinds | **9** (DAMAGE, HEAL, MODIFY, STATUS, CLEANSE, DISPEL, REVEAL, KNOCKDOWN, CHAIN_DAMAGE) |
| World-mutating actions (teleport/summon/create/etc.) | STATUS catalog rows with `on_apply_lua` that performs the action and self-removes. Not their own kinds. |
| Multi-element damage | Multiple DAMAGE rows on the ability, each with its own type + pct |
| Migration source | `fierylib/data/effects.json` + `fierylib/data/abilities.json` reshaped per Migration Plan section |
| C++ divergence | Tagged in `docs/design/README.md`. Suggested: tag + branch `legacy-parity-snapshot` in `~/Code/mud/fierymud` before any Rust changes that conflict with parity ship |

## Decisions locked (review pass 1, 2026-05-03)

| Question | Locked |
|---|---|
| Visibility default | **`is_visible` defaults true.** Per-row column for narrative opt-out (cursed-item surprises). Stealth/Frozen/Ghost are not EffectInstances — separate ECS markers. |
| `tags` vocabulary | **Typed `EffectTag` enum.** ~12 initial variants (MAGIC, PHYSICAL, ELEMENTAL, MENTAL, DISEASE, POISON, CURSE, BLEED, BUFF, DEBUFF, NEUTRAL, ENVIRONMENTAL, HOLY, NECROTIC, ARCANE, NATURAL). Adding values = one-line migration. |
| Action granularity | **Split `ATTACK_INITIATE` from `ATTACK_CONTINUE`.** Calm/pacify use only INITIATE; paralysis/hold use both. |
| Stacking | **Strongest-or-equal wins.** Refresh duration on equal, replace on stronger, no-op on weaker. MODIFY effects stack additively (different rule, kept). |
| Source shape | **Keep current shape** (`source: EffectSource` enum + `source_ability_id Option<i32>`). No tagged-union merger; awkward at the schema level. Multi-effect abilities spawn multiple EffectInstances all sharing the same `source_ability_id`. |

## Remaining open questions

None blocking the migration. Tuning knobs we can revisit later:

- Whether `EffectTag` should grow more variants once content
  surfaces real dispel/cleanse use cases.
- Whether `source` ever needs additional per-variant FK columns
  (`source_object_zone/id` for "which scroll", etc.). Add nullable
  columns only when a use case demands it.
- Whether instant-effect types (DAMAGE / HEAL / KNOCKDOWN) should
  bypass EffectInstance entirely and resolve in the cast pipeline
  without a transient row. Cosmetic implementation choice.
