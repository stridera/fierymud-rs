# Posture and Life State

**Status:** proposal — awaiting review.

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

Not an enum on the schema. Lives entirely as **runtime ECS markers**:

| Marker | Meaning | Source |
|---|---|---|
| `Ghost` | dead, untouchable | `release` after death |
| `Stunned` | input refused for N seconds | stun-effect spawn |
| `Frozen` | admin-frozen | `freeze` command |
| (Health.hp == 0) | dying / corpse | combat death handler |

These are *not* in the schema because they're transient — Ghost
persists across logouts (admin restore needed), Stunned/Frozen don't.
A separate `Characters.is_ghost Bool` already exists; that's the only
persisted bit needed. Stunned/Frozen are recreated from active effects
on login if applicable.

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
3. Drop the `Position` enum and any column that referenced it.
4. Runtime: `PostureKind::Fighting` variant gets removed; combat
   detection switches entirely to "has `Fighting` component".

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

## Open questions

1. **`Posture::Fighting` removal.** Pure cleanup but it's referenced
   in a few places. Worth verifying every read site at migration time.
2. **Restraint via posture.** Some content might want "lying prone"
   as a separate posture from "resting" (knockdown vs voluntary
   rest). Two states or one? Recommendation: one — `RESTING` covers
   both, with the difference encoded as an active effect (e.g.
   `KnockedDown` debuff with a duration). Avoids enum bloat.
3. **`Ghost` persistence.** Today it persists via
   `Characters.is_ghost`. Is this the right storage? Or does ghost-on-
   reconnect read from "did you die without releasing" by checking
   for active effects? I'd keep the column — explicit.
4. **`Frozen` persistence.** Today the marker is volatile. Should
   admin-freeze persist across reconnect? If so, that's a column on
   Characters. Probably yes — being able to freeze a logged-out
   griefer is the use case.
