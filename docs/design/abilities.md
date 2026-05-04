# Abilities

**Status:** locked except where noted (review pass 1, 2026-05-03).

## Design intent

Every spell, skill, song, and chant is an `Ability` row. The runtime
exposes one dispatch path (`invoke_ability`); the variation between
"cast fireball" / "skill backstab" / "perform inspire" is data, not
code branches. This proposal cleans up the schema so the data is
self-sufficient (no `pub(crate) const X_COST` constants in code, no
formula language for things that are constants).

## Scope: room only

A MUD operates room-by-room. There is no map. Every AOE-style ability
fires inside the caster's current room. The `TargetScope` enum drops
`CONE`, `LINE`, `CHAIN` — they were a tabletop translation that
doesn't map. The new shape:

```
TargetScope:
  SELF             - caster only
  SINGLE           - one resolved target (player / mob / item-in-inv per `OBJECT_INV`)
  ROOM_ALLIES      - caster + every group member in the room
  ROOM_ENEMIES     - every Mob (or PK-eligible Player) in the room except allies
  ROOM_ALL         - everyone in the room except the caster
  ROOM_ENVIRONMENT - room aura; one EffectInstance attached to the room
                     entity, tick re-applies to occupants on entry
```

Friendly-fire policy: `ROOM_ENEMIES` excludes party members
(`group_root`). `ROOM_ALLIES` excludes mobs unless explicitly tagged
allied. `ROOM_ALL` is admin / chaos territory.

## Schema additions to `Ability`

| Column | Type | Default | Notes |
|---|---|---|---|
| `stamina_cost` | Int | 0 | Replaces the 20 `*_COST` consts. Used by SKILL kind only. SPELLs use slots; SONG/CHANT use cooldown. |
| `target_scope` | TargetScope | SINGLE | Replaces today's free-string `targeting.scope` |
| `valid_targets` | TargetType[] | `[]` | enum array — see below |
| `weapon_required` | WeaponClass[] | `[]` | empty = any/none; otherwise restricts to those weapon classes |
| `posture_required` | Posture | STANDING | min posture to invoke |
| `combat_only` | Bool | false | refused outside combat |
| `noncombat_only` | Bool | false | refused while engaged |
| `cooldown_ms` | Int | 0 | already exists. Primary resource gate for SONG/CHANT. |
| `cast_time_rounds` | Int | 0 | already exists; 0 = instant |
| `is_magical` | Bool | true | Gates ward engagement at combat pipeline step 5. False for mundane on-hit abilities (lit-torch fire, applied poison). See [combat.md](combat.md) "Armor vs ward — independent axes". |

### `TargetType` enum (the per-ability validity filter)

```
SELF
ALLY_PC
ALLY_NPC          # tame / charm / pet target
ENEMY_PC
ENEMY_NPC
ANY_PC
ANY_NPC
OBJECT_INV        # carried item
OBJECT_ROOM       # item in current room
ROOM              # the caster's room itself (RoomEnvironmentalEffect spawning)
```

`valid_targets` is a list — an ability that works on either an enemy
or a friendly is `[ENEMY_PC, ENEMY_NPC, ALLY_PC]`. `ROOM_ALLIES`
target_scope still needs `ALLY_PC` in the validity filter.

### Drop from `Ability`

- `tags String[]` — kept (used for filtering listings).
- `targeting.scope String` — dead, replaced by `target_scope`.
- the legacy "use" / "cast" / "perform" verb hint — runtime picks the
  verb from `kind` enum.

## Resources — circles, stamina, cooldowns (no mana)

FieryMUD has always used **Vancian magic** — spell slots tied to
class circles, memorized while resting, consumed on cast. The
schema already models this fully (`SpellSlotProgression`,
`ClassAbilityCircles`, `RaceSpellSlotBonus`, `Ability.classes`
JSON / `ClassAbilities.circle` for per-spell circle assignment).
The runtime already has `MemorizedSpells` with `prep_secs_remaining`
and `ready` flags. We're keeping all of it.

There is no mana. Not now, not later. The Vancian model is more
characterful, more interesting to play, and perfectly served by
the existing tables.

### Resource gate per ability kind

| Ability kind | Gate | Notes |
|---|---|---|
| **SPELL** | Circle + memorized slot | `cast <spell> <target>` consumes one *ready* slot of the spell's class-circle. Slots regenerate by `memorize <spell>` while Sleeping / Resting / Sitting (handled by `regen::memorize_tick`). Class + level + race set circle availability and slot count. |
| **SKILL** | Stamina | `Ability.stamina_cost` is the per-skill drain (replaces 20 `*_COST` consts). Out of stamina → "you're too winded to X." |
| **SONG / CHANT** | Cooldown only | At-will with `cooldown_ms` gating re-use. Bards and mystics don't memorize — that's the whole point of the class. v2 can add stamina cost if class differentiation needs it. |

### Spell circles in detail

Per-class circle assignment lives in two places today:

- `Ability.classes JSON` shorthand: `{"Pyromancer": 7, "Sorcerer": 3}`
- `ClassAbilities(class_id, ability_id, circle, proficiency_cap)` normalized table

Those duplicate. We treat `ClassAbilities` as the source of truth
and propose dropping the JSON shorthand in
[schema-reconciliation.md](schema-reconciliation.md) — one less
place to keep in sync at content-author time.

The `cast` pipeline:

1. Parse `cast <spell> [target]`.
2. Resolve the spell to an `Ability` row (kind = SPELL).
3. Look up `ClassAbilities(player.class, spell)` — if no row, refuse
   ("your class can't cast that").
4. Look up the player's `MemorizedSpells` for a *ready* entry of
   that ability — if none, refuse ("you haven't memorized that").
5. Consume the slot (entry removed from MemorizedSpells; player
   re-memorizes via `memorize <spell>` to refill).
6. Continue with target resolution + restrictions + saving throw +
   effect application.

### Memorize / forget

`memorize <spell>` adds a fresh entry to MemorizedSpells with
`ready = false` and `prep_secs_remaining = ability.memorization_time +
class_base_prep`. The `regen::memorize_tick` ticks down once per
real second while the player is in a low-activity posture. When
`prep_secs_remaining = 0`, `ready` flips true.

Slot caps are enforced at memorize time: if you already have N
slots used in circle C and the limit is M, you can't memorize
another circle-C spell until one is consumed.

`forget <spell>` removes the most recent ready entry of that ability
(or all entries if `forget all`).

`slots` displays per-circle `used / max` with a ready-vs-preparing
breakdown.

## Schema cleanup tied to this proposal

- Remove the per-callsite stamina constants. The combat verb file's
  imports of `ATTACK_COST`, `KICK_COST`, `BASH_COST`, `BANDAGE_COST`,
  `LAYHANDS_COST`, `RESCUE_COST`, `DISARM_COST`, `HITALL_COST`,
  `DOORBASH_COST`, `BACKSTAB_COST`, `SPRINGLEAP_COST`, `GOUGE_COST`,
  `REND_COST`, `ROAR_COST`, `STOMP_COST`, `TRIPUP_COST`, `SWEEP_COST`,
  `ROUNDHOUSE_COST`, `THROATCUT_COST`, `BERSERK_COST` all go.
  `LAYHANDS_COST` becomes `Ability(name="lay_hands").stamina_cost`,
  etc.

## Runtime

```rust
pub(crate) fn invoke_ability(world, player, args, ability_kind, verb) {
    let def = lookup_by_name_with_kind(world, args[0], ability_kind);
    gate_role_and_known(world, player, def);
    gate_combat_state(world, player, def);
    gate_posture(world, player, def);
    gate_cooldown(world, player, def);
    gate_weapon(world, player, def);                   // new
    gate_resource(world, player, def);                 // single pool today
    let targets = resolve_targets(world, player, args, def);   // returns Vec<Entity>
    if !pass_target_validity(world, player, &targets, def) { return; }
    if !pass_restrictions(world, player, &targets, def) { return; }
    drain_resource(world, player, def);
    set_cooldown(world, player, def);
    let formula_ctx = build_formula_ctx(world, player);
    for target in targets {
        let saved = roll_saving_throw(world, player, target, def);
        apply_effects(world, player, target, def, &formula_ctx, saved);
        emit_messages(world, player, target, def);
    }
    bump_use_skill_quest_progress(world, player, def.id);
}
```

`resolve_targets` is the only place `target_scope` enters the
runtime. SELF / SINGLE / ROOM_*: each returns its respective list.

## Schema for ability messages

Already done — `AbilityMessages` carries
`success_to_caster` / `_self` / `_victim` / `_room` / `_self_room`,
plus `wearoff_*`. Per-target emit happens inside the
`for target in targets` loop. Single-target abilities just have a
target list of length 1.

## Examples

**Single-target damage spell** (`magic_missile`, kind=SPELL):
```yaml
ability:
  name: magic_missile
  kind: SPELL
  classes:
    - { class: Sorcerer, circle: 1 }
    - { class: Pyromancer, circle: 1 }
  target_scope: SINGLE
  valid_targets: [ENEMY_PC, ENEMY_NPC]
  effects:
    - kind: DAMAGE
      damage_amount: "level + 1d4"
      damage_type: FORCE
```
No `stamina_cost` — the slot consumption IS the cost. Memorize a
circle-1 slot; cast burns it.

**Self-buff spell** (`bless`, kind=SPELL):
```yaml
ability:
  name: bless
  kind: SPELL
  classes:
    - { class: Cleric, circle: 1 }
  target_scope: SELF
  valid_targets: [SELF]
  effects:
    - kind: STATUS
      status: bless                        # FK to Effect catalog
      override:
        duration: "skill / 4"
```

**Room AOE spell** (`fireball`, kind=SPELL):
```yaml
ability:
  name: fireball
  kind: SPELL
  classes:
    - { class: Pyromancer, circle: 4 }
    - { class: Sorcerer, circle: 5 }
  target_scope: ROOM_ENEMIES
  valid_targets: [ENEMY_PC, ENEMY_NPC]
  effects:
    - kind: DAMAGE
      damage_amount: "level + 6d6"
      damage_type: FIRE
      damage_pct: 70
    - kind: DAMAGE
      damage_amount: "level + 6d6"
      damage_type: FORCE
      damage_pct: 30
```

**Item-targeting spell** (`identify`, kind=SPELL):
```yaml
ability:
  name: identify
  kind: SPELL
  classes:
    - { class: Sorcerer, circle: 2 }
    - { class: Mystic, circle: 1 }
  target_scope: SINGLE
  valid_targets: [OBJECT_INV, OBJECT_ROOM]
  effects:
    - kind: REVEAL
      reveal_kind: IDENTIFY_ITEM
```

**Combat skill** (`bash`, kind=SKILL):
```yaml
ability:
  name: bash
  kind: SKILL
  target_scope: SINGLE
  valid_targets: [ENEMY_PC, ENEMY_NPC]
  posture_required: STANDING
  weapon_required: [SHIELD]
  stamina_cost: 8                          # SKILLs cost stamina, not slots
  cooldown_ms: 4000
  effects:
    - kind: DAMAGE
      damage_amount: "skill / 3 + str_bonus"
      damage_type: BLUDGEONING
    - kind: KNOCKDOWN
      knockdown_to: SITTING
```

**Bard song** (`inspire_courage`, kind=SONG):
```yaml
ability:
  name: inspire_courage
  kind: SONG
  classes:
    - { class: Bard, circle: 2 }            # circle gates *learning*; cooldown gates *use*
  target_scope: ROOM_ALLIES
  valid_targets: [ALLY_PC]
  cooldown_ms: 60000                        # 1 min between performances
  effects:
    - kind: STATUS
      status: inspired                       # +accuracy + ward
      override: { duration: "30 + skill / 2" }
```
No memorize gate. Bard performs at-will between cooldowns. The
class-circle assignment determines *when* the bard learns the song
(level 8 if circle 2, level 16 if circle 4, etc., per
ClassAbilityCircles), not whether they need to memorize it.

## AbilityRestrictions — additional rule types

The existing AbilityRestrictions evaluator supports rule types like
`alignment`, `target_standing`, `not_blind`, `in_combat`, `not_immobilized`,
`npc_only`, `has_weapon`. Adding rule types as content needs them:

### `target_level_relative`

For "this ability only works on weaker targets" content (Calm Animal,
Charm Person, Sleep). Compares target's level against caster's via an
operator and offset:

```yaml
restrictions:
  - type: target_level_relative
    operator: <=
    value: caster.level - 3
    message: "{target.name} is too strong for {actor.you} to {ability.verb}."
```

Operators: `<`, `<=`, `==`, `>=`, `>`. The `value` field accepts the
formula evaluator's grammar so authors can write
`caster.level + caster.cha_bonus / 2` style scaling caps.

This is a **target filter**, not a prevent flag. Once a calm effect
*does* land, the prevent flags on the Effect row apply uniformly.
The level gate is on the spawn side; the prevent is on the wear side.

## Decisions locked (review pass 1, 2026-05-03)

| Question | Locked |
|---|---|
| Resource model | **No mana.** Vancian: SPELL = circle + memorized slot; SKILL = stamina; SONG/CHANT = cooldown only. Schema's `SpellSlotProgression` / `ClassAbilityCircles` / `RaceSpellSlotBonus` / `MemorizedSpells` runtime are the implementation; the existing `regen::memorize_tick` already handles slot regen during low-activity postures. |
| Class-circle source of truth | **`ClassAbilities(class_id, ability_id, circle, proficiency_cap)`** (the normalized table). Drop `Ability.classes JSON` shorthand — duplicates the same data. Documented in [schema-reconciliation.md](schema-reconciliation.md). |
| Multi-resource abilities | **One resource per ability**, picked by `kind`. No spell costs both a slot and stamina. |
| AbilityEffect kinds | **9** — DAMAGE, HEAL, MODIFY, STATUS, CLEANSE, DISPEL, REVEAL, KNOCKDOWN, CHAIN_DAMAGE. World-mutating actions (teleport, summon, create, etc.) become STATUS catalog rows with `on_apply_lua`. See [effects.md](effects.md). |

## Decisions locked (review pass 2, 2026-05-03)

| Question | Locked |
|---|---|
| Magicality flag | **`Ability.is_magical Bool default true`.** Gates ward engagement (combat pipeline step 5). Default-true because most authored abilities are spells / supernatural skills; mundane on-hit abilities (lit-torch fire, applied poison before a poison-immune target) opt out. See [combat.md](combat.md) "Armor vs ward — independent axes". |
| ROOM_ALLIES caster inclusion | **Always includes the caster.** "Mass cure light" landing on you and the party is the natural pattern; self-buff via room AOE is more common than the exception. |
| PK-eligible filtering | **Both caster and target must carry `PlayerFlag::PkEnabled`.** `ROOM_ENEMIES` cast by a player skips other players unless this two-sided check passes. Mirrors the legacy "consensual PK" model the runtime already enforces on direct attack. |
| `weapon_required` source of truth | **Column on `Ability`** (hard requirement). Drop the parallel `weapon_type` rule in `AbilityRestrictions` — those rules are for *conditional* gates, not eligibility. The `gate_weapon` step in `invoke_ability` reads the column directly; restriction evaluator no longer needs the rule type. |
| Songs / chants stamina cost in v2 | **No schema change needed.** The `stamina_cost` column lands for SKILLs. When class differentiation calls for stamina-gated songs, individual rows set `stamina_cost > 0` against the same column. Reuse, not extension. |

## Remaining open questions

1. **Concurrent invocations / cast queueing.** Today casts are
   instantaneous (`cast_time_rounds = 0` everywhere). If
   `cast_time_rounds > 0` ever ships, queueing policy needs a
   decision: refuse a second cast input mid-channel, or queue it
   for the next available window? Defer until at least one
   long-cast ability lands.
