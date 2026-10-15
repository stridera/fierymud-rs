# Scripting guide (Lua triggers and libraries)

How scripts work in fierymud-rs: what a trigger is, when each type fires and what it can see, the
Lua API, shared libraries (`require`), errors, limits, and the mistakes people keep making.

This guide is written from the code (`crates/mud-script`, `crates/mud-server/src/triggers.rs`) and
is the reference for the Rust server. Things I could not confirm from the code are marked
**(unverified)**. The in-game `help lua-*` / `help trigger-types` pages are shorter summaries; where
they disagree with this page, this page follows the code. The C++ server's docs
(`fierymud/docs/LUA_SCRIPTING*.md`) describe a different engine.

Contents: [Concepts](#concepts) - [Trigger types](#trigger-types-and-when-they-fire) -
[Arguments and numeric values](#numeric-arguments-and-the-leading-gate) -
[Return values](#return-values) - [wait()](#wait-and-the-busy-guard) - [API](#the-lua-api) -
[Script variables](#script-variables-globals-setvar-quest-variables) - [Quests](#quest-variables-and-stages) - [Libraries and require()](#libraries-and-require) -
[Errors](#errors-and-debugging) - [Sandbox](#sandbox-and-limits) - [Pitfalls](#pitfalls) -
[Workflow](#workflow)

## Concepts

A **trigger** is a Lua body stored in the `Triggers` table, keyed by `(zone_id, id)`. It has a
name, an attach type, a set of **event flags** (the types below) and optionally a numeric
argument. Builders edit triggers in Muditor (Scripts page); `fierylib/data/triggers/` holds the
committed sources and is what a full import loads.

* **Attach type** is `MOB`, `OBJECT` or `WORLD` (a room). A trigger is attached to a mob or
  object *prototype* (or a room) with a link row; every instance spawned from the prototype
  carries the trigger. Mob and item instances keep the attachments they were spawned with, so a
  catalog reload only reaches them on their next spawn; rooms are refreshed immediately.
* **Event flags** decide when the body runs. A body can carry several flags; the `event` global
  tells it which one fired (`"SPEECH"`, `"GREET"`, ...).
* **`self`** is always the entity the trigger is attached to (the listener). **`actor`** is the
  entity that caused the event (the arriver, speaker, attacker, giver, typist). For events with
  no separate causer (`LOAD`, `RANDOM`, `DEATH` with no killer) `actor` is the same entity as
  `self`. In a room trigger `self` is the room; use `self.room`-style lookups carefully (see
  [Pitfalls](#pitfalls)).
* **`object`** is nil except for `RECEIVE`, where it is the item handed over.
* Coordinates are **composite keys**: `(zone_id, id)`, e.g. `actor:has_item(30, 45)`. There are no
  legacy vnums. Legacy id `3045` is zone 30, id 45 (zone 0 is zone 1000). Wherever the API below
  says `(zone, id)`, the key may be given any of three ways: `f(30, 45)`, `f({30, 45})` or
  `f({zone = 30, id = 45})` (`zone_id` / `local_id` also work as the key names). A key that is
  not one of those raises an error naming the accepted forms. Strings such as `"30:45"` are not
  keys.
* A body runs once per fire in a **private environment**: globals you assign are visible only to
  that run (they survive a `wait()` of the same run, nothing else). To remember something between
  runs use the entity's [`globals` table](#script-variables-globals-setvar-quest-variables)
  (same mob/item/room instance, gone when it despawns), `setvar` (per prototype, saved) or quest
  variables (per player).
* Scripts run on the game thread, one at a time, with the world locked: a body can never see a
  half-applied change, and a script that calls something that would start another script (a
  kill, a spell, a spawn) has that second fire **queued until the first body returns**.

## Trigger types and when they fire

Dispatch lives in `crates/mud-server/src/triggers.rs`. "Gated" means the mob sleep/casting gate
applies (below). Types marked *not fired* exist in the schema but nothing dispatches them (a
trigger carrying only those never runs on its own).

| Flag | Attach | Fires when | `self` / `actor` | Extra globals |
| --- | --- | --- | --- | --- |
| `LOAD` | mob | The mob spawns (boot and every respawn). Not for mobs made by `room:spawn_mobile`. | the mob / the mob | |
| `GREET`, `GREET_ALL` | mob, object | A player or mob arrives in the room (identical behaviour). | listener / arriver | |
| `SPEECH` | mob, object, room | Someone `say`s in the room, or `ask`/`tell`/`whisper`s the listener. Plain `say` wakes every scripted listener in the room plus the room's own triggers. | listener / speaker | `speech` (lowercased text) |
| `SPEECH_TO` | mob | Directed speech (`ask`, `tell`, `whisper`) only; a trigger with both flags also answers plain `say`. | listener / speaker | `speech` |
| `RECEIVE` | mob | A player gives the mob an item. The script can refuse it (see [Return values](#return-values)). | mob / giver | `object` = the item |
| `COMMAND` | mob, object, room | A player types any command. Order: the room, mobs in the room, then objects the player wears, carries, and finally objects on the floor. Not run for staff or wizinvis actors. | listener / typist | `cmd`, `args`, `arg` (= `args`), and for objects `location` = `"equip"`, `"inventory"` or `"room"` |
| `FIGHT` | mob | After each combat round, for each fighter. | the fighter / its opponent | |
| `ATTACK` | mob; object (worn) | Mob: a player starts an attack on it (self = target, actor = attacker). Object: the wearer just hit something. | see left; object: self = the item | object: `victim`, `damage` |
| `DEFEND` | object (worn) | The wearer was hit. | self = the item, actor = the attacker | `victim`, `damage` |
| `HIT_PERCENT` | mob | The mob takes a hit while its HP percentage is at or below the trigger's threshold (see below). Only the first qualifying trigger runs. | mob / opponent | |
| `DEATH` | mob | Just before the mob dies. Not gated (runs even if the mob was asleep). | the mob / the killer (the mob itself if no killer) | |
| `RANDOM` | mob, object, room | A 13 second pulse (legacy `PULSE_DG_SCRIPT`). Each scripted entity gets one roll per pulse, spread across ticks. Mobs and rooms only roll while their zone holds a player, unless the trigger also has `GLOBAL`. Only the first RANDOM trigger that passes its chance runs. | the entity / the entity | |
| `GLOBAL` | any | A modifier for `RANDOM` (lifts the "zone has players" requirement), not an event. | | |
| `BRIBE` | mob | A player gives the mob coins. The first BRIBE trigger (attach order) whose minimum the coins meet runs; the mob keeps the coins either way. | mob / giver | `value` (copper), `platinum`, `gold`, `silver`, `copper` |
| `PREENTRY` | room | Before a mover enters (before `Located` changes). | the room / the mover | |
| `POSTENTRY` | room | After the mover arrives. | the room / the mover | |
| `ENTRY` | mob | The mob is about to enter a room. | the mob / the mob | `destination` (legacy `zone*100+id` number) |
| `LEAVE` | mob, room, object | Someone walks out. Order: awake non-fighting mobs in the room, the room, floor objects. | listener / the leaver | `direction` |
| `LOOK` | mob, object | Something is looked at. The look always proceeds. | the thing looked at / the looker | `arg` (word typed) |
| `GET`, `DROP`, `WEAR`, `REMOVE`, `USE`, `CONSUME` | object | The item is picked up, dropped (also fires on `put`), worn, removed, used or eaten/drunk. | the item / the player | `target` for `put` (container or contained item) |
| `GIVE` | object | The item is being handed to someone; fires before it changes hands. | the item / the giver | `victim` = the receiver |
| `CAST` | mob, object, room | A spell is cast. Order: caster's room, then the targeted object or mob. | listener / the caster | `spell` (lowercased), `victim` on the room trigger |
| `TIME`, `TIMER`, `AUTO`, `MEMORY`, `DOOR` | | *not fired* (no dispatcher yet) | | |
| `ACT`, `RESET` | | *not fired*; legacy never dispatches them either | | |

Notes:

* `DROP` and `GIVE` fire *before* the action, and the action is cancelled only when the script
  removed or moved the item itself. The body's return value is **not** read for them (`RECEIVE` is
  different: `return false` refuses the item, see Return values).
* `run_room_trigger(zone, id)` runs *every* trigger on that room whatever its flags, with `self` =
  that room. The call is deferred until the current script returns.
* Directly firing a body for testing: the `firetrig` command and `POST /api/admin/triggers/fire`
  run a body regardless of its flags.
* Wizinvis/immortal exemptions apply only to `COMMAND`.

### The mob sleep and casting gate

Every gated mob trigger follows legacy `script_driver`: if the mob is **not awake** (sleeping or
stunned) the body never starts, and if the mob is **casting** the body is parked and starts when
the cast ends. `DEATH` is exempt. Room and object listeners are never gated. A `wait()` resume is
gated the same way.

## Numeric arguments and the leading gate

Each trigger has `num_args` (the DG numeric argument). What the Rust server does with it depends
on the type; most converted bodies carry the meaning inside the body instead:

| Type | Meaning of the argument |
| --- | --- |
| `BRIBE` | Minimum payment in **copper**. Read from the `num_args` column by the dispatcher. |
| `HIT_PERCENT` | HP percentage threshold. |
| `RANDOM` | Percent chance per pulse. |
| `SPEECH`, `SPEECH_TO` | Legacy: 0 = match a whole phrase, 1 = match a word. **Not read by the server**: the converted body does its own keyword test against `speech`. |
| others | Ignored by the dispatcher. |

For `RANDOM` and `HIT_PERCENT` the converter writes the number as a **leading gate** at the top of
the body:

```lua
-- 25% chance to trigger
if not percent_chance(25) then
    return true
end
```

The dispatcher recognises exactly this shape (comment and blank lines before it are fine), reads
the number, **strips the gate** and runs the rest. For `RANDOM` it rolls the chance in Rust before
creating any Lua environment (so the common failed roll costs almost nothing); for `HIT_PERCENT`
the number is a threshold, not a coin flip. If you write a RANDOM or HIT_PERCENT body by hand,
use the same gate as the first statement. Any other first line is run as ordinary Lua (a
`HIT_PERCENT` body without a gate has threshold 100, so it fires on every hit).

## Return values

**`COMMAND`** and **`RECEIVE`** read the return value. `COMMAND` follows legacy `script_driver`
(which starts with "block"):

| What the body does | Result |
| --- | --- |
| `return true` | The command is **not** consumed: it continues to the normal handler (and later COMMAND triggers). Use this for "not my command". |
| `return false`, no return, or reaching a `wait()` | The command is **consumed**. |
| `allow_command()` then keep going | Lets the command through and the script keeps running (legacy `return 0` followed by a `wait`). |

**`RECEIVE`** follows legacy `receive_mtrigger`: a script that returns 0 cancels the give.

| What the body does | Result |
| --- | --- |
| `return false` or `return 0` | The item is **refused**: it goes back to the giver's inventory and the giver gets no `You give ...` line (the script's own output, e.g. a "refuses" echo, still shows). |
| `return true`, `return nil`, no return, any other value | The item is **accepted** and stays with the mob. |
| Reaching a `wait()` before returning | **Accepted**: the verdict is not available while the script is parked. Do the refusing before the first `wait()`. |

The body runs with the item already in the mob's inventory (so `destroy_item` and `give_item` work), and
the server moves it back on a refusal unless the script already took it elsewhere or destroyed it. With
`give all` / `give all.<name>` each item is judged on its own and the run continues past a refusal;
`give N <item>` stops at the first refusal. While an earlier `RECEIVE` run is still parked in a
`wait()` the mob cannot look at another item (the busy guard below), so the give is not made: the item
stays with the giver, the giver sees "<Mob> seems busy.", and a bulk give stops there (items 2..N stay
with the giver). The converter spells legacy `return 0` as `return true` for non-`COMMAND` types, so a
converted RECEIVE body that must refuse has to say `return false` explicitly.

Every other event ignores what the body returns. In particular a mistaken `return false` in a
`GREET` body does nothing.

## wait() and the busy guard

Bodies run inside a Lua coroutine, so `wait(n)` can pause one.

* `wait(n)`: pause `n` **real seconds**; fractions are fine (`wait(0.2)`). The server tick is 10 Hz
  (0.1 s), so `n` is rounded to whole ticks with a one-tick floor. `wait()` = 1.
* `wait_until(hour, minute)`: pause until that game time (minute defaults to 0; if it is already
  past, until the next day). One game hour is about 75 real seconds.
* `wait_hours(n)`: legacy `wait N t`; sleeps `n` **game hours** (75 real seconds each, so it is `wait(n * 75)`).
  Not to be confused with a server tick, which is 0.1 s.
* On resume the body gets its `actor`/`self`/`object`/`speech` and a fresh `time` again, and a
  fresh instruction budget. Locals and its private globals are kept.
* A parked body is dropped if its `self` or `actor` is despawned (killed, purged, quit) before the
  wake time, and **all parked bodies are lost on a server restart**.
* **Busy guard** (legacy `TRIGGER_CHECK`): while trigger `(zone, id)` on an entity is parked, the
  same trigger on that entity **cannot start again**; the new fire is skipped and counted as
  `skipped_busy` in `/api/admin/triggers/stats`. Different entities, and different triggers on
  the same entity, are independent. So a `GREET` body that waits 10 seconds ignores arrivals for
  those 10 seconds.
* Errors after a `wait()` are recorded against the trigger like any other error.
* `wait` cannot be used at the top level of a [library](#libraries-and-require); a function a
  library returns may call it.

## The Lua API

`LuaActor` is the handle for every entity: players, mobs and items. Fields that make no sense for
the kind are nil/0. Fields are read-only; assigning one raises an error. Unknown field names read
as nil **without error**, so a typo is silent. Rooms are `LuaRoom` handles.

Messages sent from Lua already end with a newline; do not add `\r\n`. `print()` is captured and is
**not** shown to players (use `actor:send` / `room:send`).

### Globals

| Name | Description |
| --- | --- |
| `get_room(zone, id)` | The room handle or nil. |
| `find_actor(keyword)` | First mob or player in the whole world whose name/keywords match; handle or nil. |
| `find_player(name)` | A player by name; handle or nil. |
| `random(low, high)` | Uniform integer, inclusive. Use this, not `math.random`. |
| `percent_chance(n)` | True n percent of the time (n clamped to 0-100). |
| `wait(n)`, `wait_until(h, m)`, `wait_hours(n)`, `allow_command()` | See above. |
| `run_room_trigger(zone, id)` | Run every trigger on that room after this script returns. |
| `timestamp()` | Game hours as an integer. |
| `get_obj_noadesc(zone, id)` | An object prototype's name without its leading article. |
| `wizard_notify(msg)` | One line on the staff channel; at most one a minute per trigger and entity. |
| `trigger_log(...)` | Write a line to the server log, prefixed with the trigger id. |
| `zone.echo(zone_id, msg)` | Message everyone in a zone. |
| `world.count_mobiles(zone, id)`, `world.count_objects(zone, id)` | How many exist now. |
| `world.find_mobile(zone, id)` | First live mob of that prototype or nil. |
| `world.destroy(x)` | Despawn a mob or item (by handle, or by name resolved in the script's context). Never players or rooms. |
| `mobiles.template(zone, id)`, `objects.template(zone, id)` | Read-only prototype: `.id .zone_id .name .shortdesc`. |
| `combat.engage(target)` / `combat.engage(attacker, target)` | Start a fight (peaceful rooms respected). Target is a handle or a name in the room. |
| `combat.rescue(victim)` / `combat.rescue(rescuer, victim)` | The first fighter hitting the victim switches to the rescuer. |
| `skills.set_level(actor, name, level)` | Grant or set a skill (0-1000 scale). |
| `skills.execute(caster, name, target?)` | Use a skill (target handle or name). |
| `spells.cast(caster, name, target?, level?)` | Cast a spell. The level is accepted but ignored; the caster's own level is used. |
| `Effect.Name` | Effect-name constant, e.g. `actor:has_effect(Effect.Sanctuary)` (just the lowercased name). |
| `time.*` | Clock snapshot: `stamp` (unix seconds), `hour` 0-23, `minute`, `day`, `month`, `year`, `month_name`, `season`, `is_night`, `is_day`. Fresh at every fire and resume. |
| `globals` | The script-variable table of the entity the trigger is attached to (`self`): shared by all its triggers, kept between fires, dropped with the entity. See [Script variables](#script-variables-globals-setvar-quest-variables). |
| `event` | Name of the firing event. |
| `print(...)` | Captured to the run's output buffer. |

Standard Lua: `string`, `table`, `math`, `utf8`, `coroutine` (read-only copies), `pairs`, `ipairs`,
`next`, `select`, `type`, `tostring`, `tonumber`, `pcall`, `xpcall`, `error`, `assert`,
`setmetatable`/`getmetatable` (not on `_G`/strings), `collectgarbage`, `rawget`/`rawset`/`rawequal`/
`rawlen`. `string.find/match/gmatch/gsub` reject patterns that are too complex. Removed: `os`, `io`,
`debug`, `package`, `load`, `loadstring`, `loadfile`, `dofile`, `string.dump`.

### Actor and item fields

`id` / `local_id`, `zone_id`, `name`, `alias`, `shortdesc`, `room` (the room it is in; for a room
trigger there is no bare `room` global, see [Pitfalls](#pitfalls)), `worn_by` (for a worn item,
its wearer), `level`, `class` (lowercase), `race`, `size`, `gender`, `title`, `exp`, `hp`,
`max_hp`, `maxhit`, `alignment`, `armor`/`armor_pct`, `damroll`/`attack_power`, `accuracy`,
`evasion`, `real_str`/`real_dex`/`real_con`/`real_int`/`real_wis`/`real_cha`, `position`/`stance`
(string such as `"standing"`), `flags`/`aff_flags`/`eff_flags` (active effect names, uppercase,
space-separated), `hiddenness`, `can_be_seen`, `group_size`, `group_member`, `is_player`,
`is_mob`/`is_npc`, `is_immortal`, `is_fighting`, `is_ghost`, `is_stunned`, `is_frozen`; items also
have `type`, `weight`, `cost`. Pronouns: `possessive`/`hisher`, `subjective`/`heshe`,
`objective`/`himher` (these are strings chosen by gender). `tostring(actor)` is `Actor(Name)`;
build commands from `actor.name`.

### Actor methods

| Method | Description |
| --- | --- |
| `say(msg)`, `emote(msg)`, `shout(msg)` | Room speech, a third-person action, and a world-wide shout. |
| `send(msg)` | A private line to this entity. |
| `whisper(target_name, msg)` | Private line to a named target. |
| `command(line)` | Queue `line` as if the entity typed it (runs after the current script slice, not at that line). |
| `damage(n)`, `heal(n)` | Hurt (negative `n` heals, never past max). Runs death handling on a lethal hit. `damage` returns the hit points removed. |
| `attack_all()` | Engage every player in the room. |
| `move(direction)` | Walk through that exit; silent no-op if blocked. |
| `teleport(room)` | Put the entity in that room without movement triggers. |
| `follow(leader)` | Follow `leader`. |
| `spawn_object(zone, id)` | Put a fresh item in the entity's inventory; returns it or nil. |
| `destroy_item(needle)` | Remove carried items by keyword (`all.keyword` for every match). |
| `award_exp(n)` | Add experience (players only; result clamped at 0). |
| `save()` | Checkpoint the player to the database on the next autosave pass. |
| `set_flag(name, on)` | Toggle a mob behaviour flag, e.g. `sentinel`. |
| `cast(spell, target?)`, `chant(name, target?, level?)`, `perform(name, target?, level?)`, `breath_attack(element, target?)` | Use spells, chants, songs and dragon breath. |
| `has_effect(name)`, `has_effect_named(name)`, `get_has_spell(name)`, `get_flagged(flag)`, `get_eff_flagged(flag)` | Effect/flag queries. `has_effect` matches an effect name exactly (case-insensitive); `has_effect_named` ignores case, spaces and punctuation and accepts a prefix. |
| `has_skill(name)`, `get_skill(name)`, `set_skill(name, prof)` | Skill queries and setter (0-1000). |
| `has_item(zone, id)`, `has_equipped(zone, id)`, `get_worn(slot)` | Inventory queries; `get_worn` returns the item in a slot label or nil. |
| `get_people()`, `get_people(zone, id)` | Everyone in the entity's room as a list (safe to iterate while changing things), or one character of that prototype. |
| `get_objects(zone, id)`, `get_mexists(zone, id)`, `get_oexists(zone, id)` | An item in the room / how many mobs or items of that prototype exist worldwide. |
| `room_name()`, `is_god()` | Room name; true for staff. |
| `setvar(name, value)`, `getvar(name)`, `clearvar(name)` | Persistent variable bag, see below. |
| quest methods | See [Quests](#quest-variables-and-stages). |

**`setvar` / `getvar` / `clearvar`** store numbers, strings, booleans and tables (not functions or
userdata) in the `entity_variables` table through an in-memory cache flushed roughly every 10
seconds. `setvar(name, nil)` clears. They need an entity with a prototype key, so they work on
mobs, items and rooms from the catalog and are a quiet no-op on players and script-spawned
entities. Use quest variables for per-player data.

### Room handles

Fields: `id`/`local_id`, `zone_id`, `name`, `actors`/`people` (list), `actor_count`.

Methods: `send(msg)`, `send_except(target, msg)`, `send_to_adjacent(msg)`, `find_actor(keyword)`,
`find_object(keyword)`, `get_people(...)`, `get_objects(zone, id)`, `spawn_mobile(zone, id)`
(does not fire the new mob's `LOAD`), `spawn_object(zone, id)`, `purge()` (despawns every mob in
the room), `teleport_all(room)`, `weather()`, `temp()`, `sector()`, `is_outdoor()`,
`setvar`/`getvar`/`clearvar`, `exit(direction)` and `at(function)`.

`room:at(fn)` runs `fn` once, immediately, "in" that room (DG `at <room> <cmd>`): inside it
`self.room` is that room.

`room:exit(direction)` returns an exit handle (or nil) with `state()`, `hidden()`,
`set_state{ open=, locked=, hidden=, description=, keywords={...} }`, `set_key(zone, id)`
(nil clears) and `set_destination(room)`.

## Script variables: `globals`, `setvar`, quest variables

Three stores, by lifetime and owner:

| Store | Owner | Lives until | Use for |
| --- | --- | --- | --- |
| `globals.name` | the mob/item/room **instance** the trigger is attached to | that instance despawns (or the server restarts) | DG `global` variables: state shared by the triggers of one mob/item/room |
| `self:setvar(name, v)` | the **prototype** (every instance shares it) | cleared explicitly; saved to the database | settings and counters that must outlive a respawn or reboot |
| `actor:set_quest_var(...)` | one player | quest reset | per-player progress |

`globals` is the legacy DG `global <var>` store (`ScriptData::global_vars`, one per mob, item or
room, freed with it). Every trigger that fires on the same instance sees the same Lua table, in
any later fire; a `wait()` resume sees it too. Nothing else is shared: two mobs of the same
prototype have separate tables, a mob that dies and respawns starts with an empty one (a zone
reset does not touch a mob that is still alive, exactly like legacy), and the table is never
written to the database. Legacy also saved an item's globals with the item in a player's rent
file; here an item's table is not saved. Values may be anything Lua holds, including tables and
actor handles (a handle to a despawned entity reads as nothing, as everywhere else).

The owner is `self`, not `actor`: a GREET trigger on a mob stores into the mob's table, not the
arriving player's. A room trigger stores into the room. Triggers on different entities do not see
each other's table; an item's trigger cannot read the mob that carries it.

How DG maps: `global x` becomes `globals.x = x`; a read of a variable some other trigger on the
same entity declared `global` is `globals.x` (a bare `x` is a local or an undefined global, which
reads nil); `unset x` is `globals.x = nil`. DG looked a name up in the trigger's own variables
first, then the global store, and `set`/`eval` always wrote the trigger's own variable: write
`globals.x = ...` explicitly wherever the DG ran `global x` afterwards. The `remote` and
`context` commands never existed in this server's DG dialect.

Because the table outlives the fire, a guard such as `if globals.running then return end`
latches until something clears it. Clear it on every exit path (including early `return`s) and
keep waits short; a script that errors between setting and clearing leaves it set for the life of
the instance. The old `globals.x = globals.x or true` converter artefact is wrong here: assign
the real value.

Store data, not functions, in `globals`. A function stored there keeps the `actor` / `self` of
the fire that wrote it (its upvalues are captured at write time), so a later trigger calling it
sees stale handles. Store plain values (numbers, strings, booleans, plain tables, or the actor's
name) and look the entity up again in the reading trigger.

## Quest variables and stages

Per-player progress lives in the player's script variables. Quest names are free-form strings.

| Call | Meaning |
| --- | --- |
| `actor:get_quest_stage(name)` | Integer stage. **0 when the quest was never started**, so test `== 0`, not `nil`. |
| `actor:start_quest(name)` | Stage = 1. |
| `actor:advance_quest(name)` | Stage + 1 (from 0 if unset). |
| `actor:complete_quest(name)`, `actor:fail_quest(name)` | Set the completed / failed flag (the stage is untouched). |
| `actor:get_has_completed(name)`, `actor:get_has_failed(name)` | Boolean. |
| `actor:restart_quest(name)` | Wipe the quest's stage, flags and variables, then stage = 1. |
| `actor:erase_quest(name)` | Wipe everything for the quest. |
| `actor:set_quest_var(quest, key, value)` or `actor:set_quest_var("quest:key", value)` | Store a variable (values are stored as text). |
| `actor:get_quest_var("quest:key")` | `nil` when unset or empty; an **integer** when the stored text is numeric (so `v == 2`, `v + 1` work); otherwise a string. |
| `actor:active_quest(quest_zone, quest_id)` | A handle on the structured quest row with `getvar` / `setvar` / `clearvar` (players only; nil otherwise). Persisted by a periodic flush. |

`0` is truthy in Lua. `if actor:get_quest_stage("x") then` is always true; compare numbers.

## Libraries and require()

Shared code (data tables, helpers used by several triggers) lives in **script libraries**,
separate from triggers so a library cannot be attached to anything by mistake.

* Stored in the `ScriptLibrary` table (unique lowercase `name`, optional owning `zone_id`,
  `description`, `code`). Edit them on the Muditor Scripts page (Libraries tab) or commit them to
  `fierylib/data/script_libraries/*.lua` (header `-- Library: <name>`, optional `-- Zone: N` and
  `-- Description: ...`; loaded with `fierylib seed script-libraries`).
* Names are 1-64 characters of `a-z 0-9 _ - . /`.
* `require("name")` returns the library's value. Unknown name: `module 'name' not found`.

```lua
-- library "wand-quest"
local stages = { acid = { [2] = { master = {30, 13} } } }
return {
  master_of = function(element, stage)
    return stages[element][stage].master
  end,
}
```

```lua
-- a trigger
local wandQuest = require("wand-quest")
local zone, id = table.unpack(wandQuest.master_of("acid", actor:get_quest_stage("acid_wand")))
```

Semantics (as standard Lua `require`, adapted to the database):

1. **First call compiles and runs the library once**, with chunk name `lib:<name>` (errors read
   `lib:wand-quest:12: ...`), and caches its first return value (`true` if it returns nothing).
2. **The cache is shared by every trigger** in the server (one Lua state): all callers get the
   same table. That is the point (shared data), and the trap: do not keep per-player or per-call
   state in module-level variables or in the returned table. Return data and functions.
3. **Clean environment.** The library runs with the same sandboxed globals and API as a trigger
   (`world`, `mobiles`, `random`, `Effect`, `combat`, ..., read-only `string`/`table`/`math`),
   but with none of the firing context: no `actor`, `self`, `object`, `speech`, `event` or
   `globals`, and not the requiring trigger's locals or globals. Globals the library assigns stay
   in the library. `time` is read live (each access), because the library outlives the fire that
   loaded it. Functions that rely on the firing entity (`combat.engage(target)`, `actor:`
   methods on a passed-in handle) work when a trigger calls them, because that context is bound
   at call time; pass handles in as parameters.
4. **No yielding at load.** `wait()`, `wait_until()` and `wait_hours()` at a library's top level
   raise `wait() cannot be called while library '<name>' is loading ...`. A **function the
   library returns may call `wait()`**: it runs in the calling trigger's coroutine and parks that
   trigger like any other `wait`.
5. **Errors are not cached.** A library that fails to compile or run raises
   `error loading library '<name>': ...` from the `require` call, so it surfaces through the
   trigger's normal error path with the library name; the next `require` tries again.
6. **Cycles** (`a` requires `b` requires `a`) raise `circular require of '<name>'`.
7. **Reload.** The library set is loaded from the database at boot and again by `treload` /
   `POST /api/admin/triggers/reload` (which also drops the module cache, so the next `require`
   of each library re-runs the new source). Tables a running trigger already holds stay as they
   were. A server without the table logs one ERROR at boot and treats every library as missing
   (`module not found`); it never fails to boot.
8. A library can `require` other libraries.
9. `require` is not available in room-entry *condition* scripts (the read-only evaluator).

## Errors and debugging

A body that raises is recorded against its trigger:

* In-memory ring buffer: `GET /api/admin/triggers/errors?limit=N` (default 50, newest first) and
  the `scripterrors [n]` command. Rows: `trigger_zone`, `trigger_id`, `trigger_name`, `event`,
  `message` (starts with `lua error:` and names the chunk and line; a library error names
  `lib:<name>`).
* Persisted: the `script_error_log` table (type `runtime`; `suppressed` for summaries).
* **Throttling**: at most 5 warnings/database rows per trigger per 10 minutes; further failures
  are only counted, and a `suppressed N further errors` summary row is written when the window
  closes. The in-memory ring and the stats counters are not throttled.
* `GET /api/admin/triggers/stats`: fires, successes, failures and `skipped_busy` per event. A body
  that errors after a `wait()` counts as failed (it was a success until the resume raised).
* `GET /api/admin/triggers/validate[?zone=N]`: compile-checks bodies.
* `GET /api/admin/triggers` (and `/{zone}/{id}`): the catalog without bodies.
* Commands for staff: `tstat <zone> <id>`, `trighistory <target>`, `firetrig`, `lua <code>`,
  `tlist`, `treload`.
* Muditor's Scripts editor shows syntax errors and lint warnings; fix those first. A `syntax_error`
  stored on a trigger means it will fail when fired.

After saving a trigger or library in Muditor, run `treload` (or POST the reload endpoint) to make
the running game see it.

## Sandbox and limits

* No filesystem, process or debug access (see the removed list above). `require` is the
  database-backed one.
* Every fire or resume gets **about 5 million Lua instructions** and a **50 ms wall-clock**
  allowance; exceeding either aborts the body with `trigger exceeded instruction budget`. The
  budget error cannot be caught: `pcall`/`xpcall`/`coroutine.resume` re-raise it. Loops that poll
  must `wait()`.
* **64 MB** Lua heap; allocations past it fail inside the script instead of the server.
* The shared standard-library tables are read-only; a script cannot modify another trigger's
  environment.

## Pitfalls

Mistakes that have shipped, most recent first:

1. **`0` is truthy.** `if actor:get_quest_stage("q") then` is always true. Use `== 0` / `> 0`.
   Likewise an empty-string check is `== ""`, not falsy.
2. **Quest variables come back typed**: nil when unset, an integer when numeric, else a string.
   `v == "2"` is false for the number 2; compare with the number.
3. **There is no bare `room` global.** Use `self.room` (an entity's room) or, in a room trigger,
   `self` is the room: `self:send(...)`.
4. **Strings versus handles.** Spell and skill functions take a *name string* or a handle for the
   target (see each call); `object.name` is a string, `object` is a handle. `has_item` takes
   `(zone, id)` numbers, not a name.
5. **Class names are lowercase** (`actor.class == "sorcerer"`); `race` likewise.
6. **No vnums.** `self.vnum` is nil without error. Use `self.zone_id`, `self.id`.
7. **Typos are silent**: `actor.levl` is nil. Fields are read-only: `self.hp = 5` raises; use
   `damage()` / `heal()`.
8. **`local` in a branch** is scoped to that branch; declare it before the `if`.
9. **SPEECH from `say`: `actor` is the speaker; `speech` is lowercased** (ASCII) and the body must
   test keywords itself (the server does not filter). So every needle compared with `speech`
   (`string.find(speech, "2h")`, `speech == "yes"`, `string.match(speech, "t(%d+)t")`) must be
   lowercase: a capital letter in the needle can never match.
10. **`object` is nil** except in `RECEIVE`; in item events `self` is the item.
11. **A body that waits blocks itself**: the busy guard skips fires of the same trigger on the same
    entity while it is parked.
12. **Parked bodies die on restart.**
13. **Output**: `print` is not shown; do not add `\r\n`.
14. **Library state is shared**; see [require](#libraries-and-require).
15. **The `percent_chance` gate must be the first statement** to be recognised as the gate for
    `RANDOM` / `HIT_PERCENT`.
16. The DG `%get.*%` / `%time.stamp%` forms have no Lua equivalent under those names: write
    `time.stamp`, `objects.template(zone, id).name`, etc.
17. **`room.actors` / `room.people` is always a table**, so `if not room.people then` never
    fires for an empty room. Test the length: `if #room.people == 0 then`.
18. **Handles compare by what they wrap**: `actor.room == self.room` and `a == b` are true for the
    same room or actor (each access creates a new handle, which used to compare unequal).

## Workflow

1. Write the body in Muditor (Scripts page) or in `fierylib/data/triggers/` (the source of truth
   for existing triggers; edits there are loaded by the importer). Libraries: Libraries tab or
   `fierylib/data/script_libraries/`.
2. Attach it to a mob/object/room prototype in the matching editor.
3. `treload` in game, or `POST /api/admin/triggers/reload`.
4. Test with `firetrig` / `POST /api/admin/triggers/fire`, then check `scripterrors` and
   `trighistory`.

Who may edit what in Muditor is in `muditor/docs/PERMISSIONS.md` (triggers and libraries are
writable per zone for builders with a grant, everywhere for head builders and above; a library
with no zone is head builder and above only).
