# Abilities

**Status:** proposal — awaiting review.

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
| `stamina_cost` | Int | 0 | Drops the 20 `*_COST` consts in `commands.rs` |
| `mana_cost` | Int | 0 | 0 today; reserved for class differentiation |
| `target_scope` | TargetScope | SINGLE | Replaces today's free-string `targeting.scope` |
| `valid_targets` | TargetType[] | `[]` | enum array — see below |
| `weapon_required` | WeaponClass[] | `[]` | empty = any/none; otherwise restricts to those weapon classes |
| `posture_required` | Posture | STANDING | min posture to invoke |
| `combat_only` | Bool | false | refused outside combat |
| `noncombat_only` | Bool | false | refused while engaged |
| `cooldown_ms` | Int | 0 | already exists |
| `cast_time_rounds` | Int | 0 | already exists; 0 = instant |

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

## Stamina/mana — single pool first

Recommendation: **one resource pool** today (`Stamina`), with
`stamina_cost` driving every ability. `mana_cost` exists in the schema
but is `0` everywhere until a class is built that explicitly wants a
mana pool. When that lands, add a `Mana(i32, i32)` component and the
runtime branches on `Ability.uses_resource: STAMINA | MANA | KI`.

That keeps content authoring clean now (one column to fill, one number
to balance) and leaves the door open.

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

**Single-target damage spell** (`magic missile`):
```yaml
target_scope: SINGLE
valid_targets: [ENEMY_PC, ENEMY_NPC]
stamina_cost: 0
mana_cost: 8
abilityEffects:
  - effect_id: <damage row>, override_params: {amount: "level + 1d4"}
```

**Self-buff** (`bless`):
```yaml
target_scope: SELF
valid_targets: [SELF]
stamina_cost: 0
mana_cost: 5
abilityEffects:
  - effect_id: <bless row>, override_params: {duration: "skill / 4"}
```

**Room AOE** (`fireball`):
```yaml
target_scope: ROOM_ENEMIES
valid_targets: [ENEMY_PC, ENEMY_NPC]
stamina_cost: 0
mana_cost: 20
abilityEffects:
  - effect_id: <fire damage row>, override_params: {amount: "level + 6d6"}
```

**Item targeting** (`identify`):
```yaml
target_scope: SINGLE
valid_targets: [OBJECT_INV, OBJECT_ROOM]
stamina_cost: 0
mana_cost: 5
abilityEffects:
  - effect_id: <identify reveal row>
```

**Room aura** (`light room`):
```yaml
target_scope: ROOM_ENVIRONMENT
valid_targets: [ROOM]
stamina_cost: 0
mana_cost: 3
abilityEffects:
  - effect_id: <room-light row>, override_params: {duration: "skill * 60"}
```

## Open questions

1. **Should ROOM_ALLIES include the caster?** ("Mass cure light"
   landing on you and your party.) Recommendation: yes, always —
   self-buff via room AOE is a common pattern and easier to reason
   about than the exception.
2. **PK-eligible filtering.** When does `ROOM_ENEMIES` cast by a
   player include other players? Today PK is opt-in via flag. Probably:
   only when both caster and target have `PlayerFlag::PkEnabled`.
3. **`weapon_required` granularity.** WeaponClass at the row level
   (PIERCING, SLASHING, …) vs the existing `weapon_type` rule in
   `AbilityRestrictions`. Two ways to spell the same thing. I'd prefer
   the column on `Ability` since it's a hard requirement, not a
   conditional bonus.
4. **Concurrent invocations.** Should a player be able to queue a
   second cast while the first is mid-cast-time? Today it's
   instantaneous; if `cast_time_rounds > 0` ever ships, queueing
   policy needs a decision.
5. **Multi-resource abilities.** Anything that costs both stamina and
   mana? I'd default to "abilities pick one resource" — keeps the
   model tractable.
