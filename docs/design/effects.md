# Effects

**Status:** proposal — awaiting review.

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
| `tags` | String[] | `["magic", "fire"]` etc. — drives dispel filtering |
| `prevents` | Action[] | enum array — see below |
| `default_duration_secs` | Int? | When the spawning ability didn't override |
| `default_strength` | Int? | Same |
| `is_dispellable` | Bool | False for environmentals, weather-driven status, etc. |
| `is_visible` | Bool | False = hidden from `effects` listing (subliminal buffs) |
| `on_apply_lua` | String? | Lua source — runs once when the instance lands |
| `on_tick_lua` | String? | Lua source — runs every effect tick |
| `on_remove_lua` | String? | Lua source — runs when instance expires/is dispelled |

### `Action` enum (the new prevents shape)

```
SPEAKING
CASTING
MOVEMENT
MELEE         # blocks ordinary swings
ABILITY_USE   # blocks `cast` / `skill` invocation (bigger hammer than CASTING)
TARGET_CHANGE # locks the target you're already on
ITEM_USE      # blocks `quaff` / `recite` / `wave` / `tap`
LOOK          # blind
```

A row's `prevents` is the union of categories that effect category
blocks. `web` would be `[MOVEMENT]`. `paralysis` would be `[MOVEMENT,
MELEE, ABILITY_USE, TARGET_CHANGE]`. `silence` is `[CASTING]`.

### EffectInstance (runtime ECS, also persisted)

| Field | Notes |
|---|---|
| `effect_id` | FK to `Effect.id` (renamed from `kind` to make it clear) |
| `applied_to` | character / mob entity ref |
| `strength` | Per-instance numeric (heal amount, modifier delta, …) |
| `expires_at` | Or remaining secs; permanent if null |
| `source` | enum: spell, item, environment, racial, admin |
| `source_ability_id` | FK to `Ability.id` if cast by one — used to fetch the wearoff message |
| `params` | Json | per-instance kv (modify-target name, redirect-pct, etc.) |

`EffectInstance.kind` is renamed `effect_id` to drop the legacy term.

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

| Category | Rule |
|---|---|
| BUFF, same effect from same source | Refresh duration; do not stack strength. |
| BUFF, same effect from different sources | Strongest wins (higher `strength`); refresh on equal. |
| DEBUFF, same effect from any source | Refresh duration; strongest strength wins. |
| MODIFY (e.g. +str) | Stack additively; each instance is its own delta tracked via the existing `ModifyDelta` companion record. |
| ENVIRONMENTAL (room aura) | Always re-applied on entry; expires on exit or when room effect is removed. |

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

## Open questions

1. **Visibility default.** Do BUFFs default to visible (player sees
   them in `effects`) and DEBUFFs to visible-with-counter? Or keep
   `is_visible` per-row and let content authors choose?
2. **`tags` vocabulary.** Should `tags` be a free String[] (today's
   shape) or a typed enum? Free-string is more flexible, enum is
   harder to typo. I'd default to free-string with a documented
   convention list.
3. **`Action::MELEE` granularity.** Do we need `MELEE_INITIATE`
   (refuse to start fights) separately from `MELEE` (refuse to
   continue)? Or is one flag enough?
4. **Concurrent identical buffs.** When two clerics cast bless on the
   same target, do they each leave their own EffectInstance (current
   shape, requires GC) or does the first refresh and the second
   silently no-op?
5. **`source_ability_id` vs richer source.** Today `EffectSource` is an
   enum (Spell, Item, Environment, …) and we tack `ability_id` on
   separately. Worth merging into a single tagged-union column?
