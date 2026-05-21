# Rest System: Inns, Camping, and Repose

**Status:** ready for implementation (vocabulary locked in `/muditor/CONTEXT.md` under "Resting and Repose", design grilled 2026-05-17). Numbers marked **TUNABLE** are explicit placeholders — surface real values during implementation playtest.

## Design intent

Casual players should feel no penalty for leaving the game — no XP loss, no gear loss, no death on disconnect. At the same time, players who *invest* in their rest (camp properly, rent a room) should be rewarded. The reward shape is borrowed from MMO "rested XP" plus a short post-wake regen window, layered with optional per-source Effect attachments (e.g. wizard academy grants `FOCUS`).

Vocabulary lives in [`/muditor/CONTEXT.md`](../../../muditor/CONTEXT.md) under **Resting and Repose**: `Repose`, `RestSource`, `Refreshed Effect`, `Wake Effect attachment`.

## Data model

### New columns on `Characters`

| Column | Type | Default | Purpose |
|---|---|---|---|
| `repose` | Int | 0 | Sticky bonus-XP pool. Spent at XP-gain time; never decays. |
| `restSource` | RestSource | NONE | Kind of prepaid rest awaiting consumption. |
| `restTier` | Int | 0 | Quality 0-3 of the prepaid rest. |

### New columns on `Rooms`

| Column | Type | Default | Purpose |
|---|---|---|---|
| `isInn` | Bool | false | This room sells rentable rest. |
| `innName` | String? | null | Display name (e.g. "The Rusty Boar"). |
| `innTiers` | Json? | null | Available tiers: `[{name: string, tier: 1-3, fee: int}]` |

### New columns on `Objects`

| Column | Type | Default | Purpose |
|---|---|---|---|
| `campKitTier` | Int? | null | If set (1=basic, 2=premium), this Object is a consumable camp kit. |

### New enum

```prisma
enum RestSource {
  NONE
  QUIT
  CAMP
  INN
  HOUSE
}
```

### New junction tables

```prisma
model RoomWakeEffects {
  roomZoneId   Int
  roomId       Int
  effectId     Int
  modifierData Json @default("{}")
  duration     Int      // seconds the granted Effect lasts on the character
  minTier      Int?     // if set, attachment only applies when rented at this tier or higher

  Room   Rooms  @relation(fields: [roomZoneId, roomId], references: [zoneId, id], onDelete: Cascade)
  Effect Effect @relation(fields: [effectId], references: [id], onDelete: Cascade)

  @@id([roomZoneId, roomId, effectId])
}

model ObjectWakeEffects {
  objectZoneId Int
  objectId     Int
  effectId     Int
  modifierData Json @default("{}")
  duration     Int

  Object Objects @relation(fields: [objectZoneId, objectId], references: [zoneId, id], onDelete: Cascade)
  Effect Effect  @relation(fields: [effectId], references: [id], onDelete: Cascade)

  @@id([objectZoneId, objectId, effectId])
}
```

## Lifecycle

### Acquire source

| Trigger | Effect |
|---|---|
| `rent <tier-name>` in an `isInn=true` room | Look up tier in `innTiers`, charge fee from `gold`, set `restSource=INN`, `restTier=<chosen>`. |
| `camp [<kit-name>]` outdoors (existing `cmd_camp`) | After 35s setup completion: consume named kit (if any), set `restSource=CAMP`, `restTier = computeCampTier(...)`. |
| Disconnect / `quit` in a HOUSE-owned room | Set `restSource=HOUSE`, `restTier = <house quality>` (see Open / deferred below). |
| Disconnect / `quit` anywhere else (and current source is NONE) | Set `restSource=QUIT`, `restTier=0`. |

Renting a new source *overwrites* `restSource` and `restTier`. Pool (`repose`) is NEVER cleared by acquisition — only by XP-gain consumption.

### While offline

Server computes Repose accrual at next login (not as a background tick). Pseudocode:

```
elapsed_hours = (now - lastLogout) / 3600
rate = ratePerHour(restTier)
cap = capPercent(restTier) * xpForNextLevel(character.level)
gained = floor(elapsed_hours * rate)
repose = min(repose + gained, cap)
```

### Login spawn location

```
if restSource in {CAMP, INN, HOUSE}:
    spawn at currentRoom (where logged off)
elif restSource in {QUIT, NONE}:
    if (now - lastLogout) >= 30min:
        spawn at recallRoom (Characters.recallRoomZoneId/Id, fallback race home)
    else:
        spawn at currentRoom
```

Spawn position is set BEFORE the player sees the room — never relocate them after they've started interacting.

### First XP gain after login (the "wake")

```
on_xp_gain(character, base_xp):
    if character.restSource not in {NONE, QUIT}:
        spawn Refreshed Effect (strength = character.restTier,
                                duration = 30 min real-time TUNABLE)
        apply wake-effect attachments:
            if restSource == INN:
                for row in RoomWakeEffects where (zone,id) == currentRoom AND (minTier == NULL OR minTier <= restTier):
                    spawn Effect from row.effectId with row.duration / row.modifierData
            elif restSource == CAMP:
                # kit Object was consumed at camp completion; rows looked up at that moment
                # and pinned to the character as pendingWakeAttachments (or replayed from a
                # transient state). Simplest: at camp completion, copy the kit's
                # ObjectWakeEffects rows into a transient list on the character and apply them here.
                for row in pending_wake_attachments:
                    spawn Effect from row.effectId ...
            elif restSource == HOUSE:
                for row in ObjectWakeEffects where Object is a bed in currentRoom:
                    spawn Effect from row.effectId ...

    if character.restSource != NONE:
        character.restSource = NONE
        character.restTier = 0

    # Spend Repose on this XP gain and onward:
    multiplied = apply_repose_multiplier(character, base_xp)
    return multiplied
```

`apply_repose_multiplier`:

```
REPOSE_MULTIPLIER = 2.0  # TUNABLE
bonus = base_xp * (REPOSE_MULTIPLIER - 1)
drawn = min(bonus, character.repose)
character.repose -= drawn
return base_xp + drawn
```

## Tier table (TUNABLE)

| Tier | Source examples | Cap (% of next level) | Fill / hour | Time to cap |
|------|----------------|----------------------|-------------|-------------|
| 0 | NONE / QUIT | 0 | 0 | — |
| 1 | basic inn / bare camp | 10 | 2.5 | 4 h |
| 2 | suite inn / ranger camp | 25 | 5.0 | 5 h |
| 3 | penthouse / ranger + premium kit | 50 | 10.0 | 5 h |

## Camp tier computation

Resolved at `cmd_camp` completion (in `mud-server/src/camp.rs`):

```rust
let mut tier = 1;
let outdoors_self = matches!(class, Class::Ranger | Class::Druid);
let outdoors_party = group_members(player).any(|m| matches!(m.class, Class::Ranger | Class::Druid));
if outdoors_self || outdoors_party {
    tier += 1;          // at most one fieldcraft bonus, never double-count
}
if let Some(kit_obj) = consumed_kit {
    tier += kit_obj.campKitTier; // 1 basic, 2 premium
}
tier.min(3)
```

The hard-coded `Class::Ranger | Class::Druid` check is **temporary scaffolding** per the project's "Data Over Code" rule. Follow-up: add `CharacterClass.campcraftBonus Int` (default 0) and replace the match with `class.campcraftBonus > 0`. Track in fierymud-rs remaining-work as a clean-up.

## Refreshed Effect

Single row in the `Effect` table seeded by fierylib (RR3):

| Field | Value |
|---|---|
| `name` | `Refreshed` |
| `effectType` | `status` |
| Lua `on_apply` | store `_strength` (1-3) and capture base regen rates |
| Lua `on_tick` (every 10 ticks = 1s) | add a per-tick `RegenBonus` of `base_hp_regen * 0.25 * strength` and `base_stamina_regen * 0.25 * strength` (TUNABLE) |
| Lua `on_remove` | drop the bonus |
| Default `duration` (attachment) | 1800 seconds (30 min real-time) TUNABLE |

If the player triggers a second wake before Refreshed expires (rare — would require renting again mid-session and consuming), the new Refreshed *replaces* the old (last-write-wins on the CharacterEffects row).

## Schema migration order

1. **fierylib seeder for `Refreshed` Effect row.** Runtime spawns it by name — must exist before runtime tries.
2. **`RestSource` enum** added to Prisma schema.
3. **`Characters` columns** (`repose`, `restSource`, `restTier`).
4. **`Rooms` columns** (`isInn`, `innName`, `innTiers`).
5. **`Objects.campKitTier`.**
6. **`RoomWakeEffects` and `ObjectWakeEffects` junction tables.**
7. `bun run db:generate` (muditor) AND `poetry run prisma generate` (fierylib).
8. `bun run db:migrate` to apply to dev DB.
9. fierylib importer round-trips legacy data with defaults for the new columns.
10. fierymud-rs sqlx queries updated for new columns; runtime wires lifecycle and commands.

## Commands

| Command | Behavior |
|---|---|
| `rent` | In `isInn` room: list available tiers from `innTiers`. Otherwise: "There's nothing to rent here." |
| `rent <tier-name>` | In `isInn` room: charge fee, set `restSource=INN`, `restTier=<tier of chosen>`. Confirm if tier > 1: "Renting the penthouse for 50gp. Confirm? (y/n)". |
| `camp` | Existing: 35s setup. On completion: `restSource=CAMP`, `restTier = computeCampTier(no_kit)`. |
| `camp <kit-name>` | 35s setup. On completion: consume the named kit Object, `restSource=CAMP`, `restTier = computeCampTier(with_kit)`. Kit is NOT consumed if camp is interrupted. |

## Edge cases handled

| Scenario | Behavior |
|---|---|
| Death while `repose > 0` | Pool stays. `restSource` stays. Death does not wake. |
| Logout during combat | Allowed (per current fierymud-rs behavior). Source becomes QUIT (unless inn-rented). |
| Camp interrupted (combat / movement during 35s setup) | Kit NOT consumed. `restSource` unchanged. |
| Downgrade rent (player rents T1 over an existing T3) | Source overwritten to T1. Player's choice, no refund. |
| Multi-session non-XP logins | Source persists across login sessions. Only first XP gain consumes. |
| Rent without enough gold | Rent refused. Source unchanged. |
| Quit then return within 30 min | Spawn at currentRoom (grace window). No Repose, no Refreshed. |
| Two characters in inn room | Each rents independently. |
| Repose pool full, rent again | Player still charged the fee. No refund. (Caveat emptor.) |

## Open / deferred

- **Player housing schema.** `restSource=HOUSE` is reserved in the enum, but housing data model is not designed here. Mark as RR-future; treat HOUSE detection as a no-op until housing is implemented.
- **Group / party concept.** "Ranger in party" bonus depends on a group API in fierymud-rs. Verify what exists in `mud-server/src/` (search for `Group`, `Party`, `group_id`). If absent, the camp tier formula falls back to self-class only until parties land.
- **Builder UI in Muditor.** Inn config columns, wake-effect attachment editors. Post-runtime concern; for v1, seed example inn data via fierylib (RR5) and let builders edit via direct DB until UI catches up.
- **Hard-coded class check → `CharacterClass.campcraftBonus`.** Temporary scaffolding per the rules; file a follow-up after v1 ships.
- **Inn refusal for current source.** A player with an active T3 source typing `rent basic` will downgrade. We could warn "You already have a penthouse stay queued; renting basic will overwrite. Confirm?" but it's out of scope for v1.
