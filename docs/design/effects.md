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

Stays a junction table linking `Ability` to `Effect`. `override_params`
JSON keeps the per-ability overrides for `duration` / `strength` /
`amount` formulas (these legitimately vary per ability + per caster
level), but the **flag override is gone** — the ability points at the
specific Effect row by id.

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

1. Add the new columns on `Effect`. Drop `prevents_speaking` /
   `prevents_casting` / `prevents_movement` once `prevents` is
   populated.
2. fierylib seeder: split the generic `status` row into per-flag rows
   for every flag the legacy data references. Re-point existing
   `AbilityEffect` rows.
3. Rename `EffectInstance.kind` → `effect_id` in the runtime
   component (and any persistence).
4. Drop `AbilityEffect.override_params.flag` — the only remaining
   `override_params` keys are `duration`, `amount`, `strength`,
   `target` (formula or constant).

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
