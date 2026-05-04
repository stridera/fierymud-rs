# Posture and Life State

**Status:** locked except where noted (review pass 1, 2026-05-03).

## Design intent

Today the schema's `Position` enum is overloaded: it conflates
*voluntary posture* (Standing / Sitting / Resting / Sleeping /
Kneeling) with *incapacitation* (Dead / MortallyWounded /
Incapacitated / Stunned). The runtime already split these — `PostureKind`
component for the former, separate ECS markers (`Ghost`, `Stunned`,
plus a low-HP check) for the latter. Schema should mirror what the
runtime already does cleanly.

## Proposal

Replace `Position` with two distinct concepts:

### `Posture` — voluntary state

```
Posture:
  STANDING
  KNEELING
  SITTING
  RESTING
  SLEEPING
```

Single enum column on `Characters` and on the runtime `Posture`
component. Players move between these via `stand` / `sit` / `rest` /
`sleep` / `kneel` commands. Pure player intent — combat, stamina
regen, etc. read this directly.

### `LifeState` — incapacitation

Not an enum on the schema. Lives entirely as **runtime ECS markers**;
the few that need cross-session persistence get a single boolean
column on `Characters`.

| Marker | Persisted | Source | Notes |
|---|---|---|---|
| `Ghost` | `Characters.is_ghost Bool` | combat death → `release` | Survives logout; needs admin or `release` to clear. |
| `Frozen` | `Characters.is_frozen Bool` (new) | admin `freeze` command | Survives logout — the use case is freezing a logged-out griefer. Cleared by admin `thaw`. |
| `Stunned` | runtime-only | stun-effect spawn | Recreated from active effects on login if the originating effect persists; otherwise dropped. |
| (`Health.hp == 0`) | implied by `hit_points` column | combat death handler | "Dying" is just hp == 0 + alive entity until the death tick fires. |

The two `Bool` columns mirror each other intentionally — both are
"admin-clearable persistent block states." Effect-driven markers
(`Stunned`) stay volatile because their backing `EffectInstance`
already round-trips through `CharacterEffects` on login.

## What to drop

- `Position` enum, everywhere.
- Any schema column that stores a Position value
  (`Mobs.defaultPosition`, `Characters.position` if it exists).
- `Mobs.defaultPosition` becomes `Mobs.default_posture Posture` with
  `STANDING` default.

## What to keep

- The runtime `PostureKind` enum already mirrors the proposed
  `Posture` enum (it has Standing/Kneeling/Sitting/Resting/Sleeping
  plus `Fighting` — which is wrong; fighting isn't a posture, it's
  the existence of a `Fighting` component. Drop `Fighting` from the
  enum).
- `Ability.posture_required` (added in [abilities.md](abilities.md))
  references the new `Posture` enum directly.

## Schema migration

1. Add `Mobs.default_posture Posture default STANDING`.
2. Re-import any existing `defaultPosition` rows by mapping
   STANDING/SITTING/RESTING/SLEEPING/KNEELING → corresponding new
   enum value. Anything else (Dead/MortallyWounded/Incapacitated/
   Stunned) maps to STANDING — those weren't really starting states
   anyway.
3. Add `Characters.is_frozen Bool @default(false)` symmetrical to
   `is_ghost`. Login spawn restores the `Frozen` marker if set.
4. Drop the `Position` enum and any column that referenced it.
5. Runtime: `PostureKind::Fighting` variant gets removed. Combat
   detection switches entirely to "has `Fighting` component";
   `set_posture` / `cmd_stand` / `cmd_sit` paths verify they
   don't gate on the dropped variant.

### Lua trigger compatibility

Some imported triggers compare `actor.position == "STUNNED"` /
`"DEAD"` / etc. After the migration `actor.position` returns only
the new `Posture` labels (STANDING / KNEELING / SITTING / RESTING
/ SLEEPING). Triggers that gated on incapacitation states need a
translation path:

- Add `actor.is_ghost`, `actor.is_stunned`, `actor.is_frozen`
  boolean accessors. The fierylib trigger-rewrite pass migrates
  `actor.position == "STUNNED"` → `actor.is_stunned`, etc.
- Keep `actor.position` returning the new Posture string. Triggers
  that *correctly* compared against STANDING/SITTING/etc continue
  to work unchanged.
- An optional `actor.life_state` accessor returning `"normal"` /
  `"ghost"` / `"stunned"` / `"frozen"` is a nicety for legacy
  triggers that switch on the entire incapacitation set; defer
  unless the migration audit shows enough call sites to justify
  it.

## Combat / posture interactions

The fixed pipeline:

| Posture | Combat behavior |
|---|---|
| STANDING | Full accuracy + evasion |
| KNEELING | -25% evasion |
| SITTING | -50% evasion, can't initiate melee |
| RESTING | -75% evasion, attacks against auto-stand the defender |
| SLEEPING | Defender can't dodge (evasion = 0); attacker auto-wakes them |

These are constants in the combat tick (a small lookup table) since
they're rules, not content. Builder-tunable mob stats compose against
them via `accuracy` / `evasion` columns (see [combat.md](combat.md)).

## Stand-up mechanics

`require_alert_posture` (the existing helper that auto-stands a
sitting/resting/kneeling player before they take an action) keeps its
behavior but reads the new enum directly. Sleeping refuses entirely
("you can't X while asleep").

Combat against a sleeping target wakes them at the same swing
(unchanged from current runtime behavior).

## Decisions locked (review pass 1, 2026-05-03)

| Question | Locked |
|---|---|
| `Posture::Fighting` removal | **Drop the variant.** Combat detection reads the `Fighting` component, not the posture enum. Migration walks the `set_posture` / `cmd_stand` / `cmd_sit` / `cmd_rest` / `cmd_sleep` paths to verify nothing gates on the dropped variant. |
| Knockdown / restraint | **Single posture, effect-driven distinction.** `RESTING` covers both voluntary rest and forced prone. A `KnockedDown` `EffectInstance` with a duration carries the involuntary part — combat / movement gates check the effect, not a separate posture. Avoids enum bloat. |
| `Ghost` persistence | **Keep `Characters.is_ghost Bool`** as the explicit persisted bit. Cleared by `release` (player) or admin restore. |
| `Frozen` persistence | **Add `Characters.is_frozen Bool`** symmetrical to `is_ghost`. Survives logout so an admin can freeze a logged-out griefer. Cleared by admin `thaw`. |
| `actor.position` Lua semantics | **Returns only the new `Posture` labels.** Incapacitation states surface via `actor.is_ghost` / `actor.is_stunned` / `actor.is_frozen` boolean accessors. fierylib trigger-rewrite pass migrates legacy `actor.position == "STUNNED"` comparisons. |

## Remaining open questions

1. **`actor.life_state` aggregator.** Cheap to add (string returning
   `"normal"` / `"ghost"` / `"stunned"` / `"frozen"`) but only
   useful if enough imported triggers want a single switch over the
   incapacitation set. Defer until the migration audit reports
   call-site counts.
