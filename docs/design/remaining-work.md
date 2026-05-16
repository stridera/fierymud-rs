# Remaining Work

**Generated:** 2026-05-12. The post-migration to-do list — only items not yet shipped. Completed work lives in `migration-plan.md` (historical tracker) and `parking-lot.md` Resolved section.

Each item has:
- **Why** — parity / improvement / balance / decision
- **Scope** — rough effort, anchor file(s)
- **Blocker** if not just "needs implementation"

## A — Combat balance review

User flagged as the next session. Combat data is wired end-to-end and tests pass, but the math hasn't been balance-tuned against the design spec.

- **A1. `hit_chance_pct` magnitudes vs spec.** Current code is the proper d100 contest (`50 + (accuracy - evasion) / 2`, clamped 1..=99), but the magnitudes weren't compared against `docs/design/combat.md` targets. Run a sweep of expected hit-rates at level brackets, compare to legacy.
- **A2. `attack_power` flat vs % multiplier.** Code adds `attack_power` as flat damage onto the weapon-dice roll. `combat.md` describes it as additive percent (`base * (1 + AP/100)`). Decide which is intended; either change is one block in `combat.rs::apply_swing`.
- **A3. `crit_chance` hardcoded to 5.** No per-mob/per-class tuning. Decide whether to promote to a schema column (`Mobs.crit_chance`, `Characters.crit_chance` or class/race-derived).
- **A4. `posture_evasion_penalty` values (10/20/25/30) need playtesting.** Replaces the legacy 2/4/5/6 AC penalty — different scale, behavior likely tilted.
- **A5. `spell_power` has no consumer.** Magical abilities route through the `is_magical` flag, but the damage-formula path doesn't read `spell_power` yet. Wire alongside the SPELL/CHANT damage step.
- **A6. `perception` / `concealment` not consumed.** Loaded on mobs + characters; no see/hide combat step exists yet. Pipeline step needed.
- **A7. `resistances` JSON per-element step pending.** Resistances are loaded on mobs + characters + objects, but the combat pipeline doesn't apply them per damage type. Step ~5 of the combat pipeline per `combat.md`.

## B — Parity-critical wires (legacy MUD feature gaps)

Features the legacy MUD had that we still need.

- **B1. Object decay tick** (`Object.timer`, `Object.decompose_timer`). Items with positive timers tick down every N seconds; at zero, the item decomposes (per `ITEM_DECOMPOSING` flag) and is destroyed. `ITEM_PERMANENT` skips the tick. Scope: new tick in `mud-server`, ~50 lines.
- **B2. Bashable doors** (`RoomExit.hit_points`). The bash skill drives a door's HP down; at zero, the door is destroyed. Needs the bash skill to exist; once it does, this is a 1-line check.
- **B3. Mob.aggressionFormula** (Lua expression for varied aggro). Replaces hardcoded `AGGR_EVIL`/`AGGR_GOOD` etc. flags with per-mob Lua. Scope: load on Mob struct + eval at the wander/aggro tick site in `mud-server`.
- **B4. RaceSpellSlotBonus** (per-race +N slots for a circle). Loaded via a new module, folded into the spell-slot cap calculation when the slot system tracks circle pools.
- **B5. LevelDefinition.permissions** (level-granted permission flags). On level-up, union the level's permission list into the character's `permissions` array. Hooks: `combat::check_level_up`.
- **B6. Object equip restrictions** (`allowed_races`, `min_size`, `max_size`). Inclusive race-list + size-band on `wear`. The schema is there; the gate goes in `equip_apply.rs` next to the existing `restricted_*` checks.

## C — Builder-authored flavor text catalogs

Schema's there. Each is a small loader + a renderer change in the relevant command.

- **C1. CombatMessage.** Hit/miss flavor variety by attack-type + weapon-type. Today `apply_swing` emits hardcoded "You hit X" lines; this catalog provides variety.
- **C2. PositionMessage.** Sit/stand/rest/sleep transition messages. Today `cmd_sit` / `cmd_stand` etc. emit hardcoded lines.
- **C3. SystemMessage.** Error-message variants by category. Today refusal lines are hardcoded.
- **C4. PositionData.** Authoritative position metadata (`appliedEffects`, `entryRequirement`, `defaultPresence`). Today `Posture` is a hardcoded match; this would make it data-driven.

## D — Stubbed consumers (markers stored, gates pending other systems)

Items where the schema → ECS pipe is wired but the consuming gameplay system doesn't exist yet. **Don't wire individually — wait until the surrounding system lands.** Listed here so they aren't forgotten when those systems ship.

- **D1. ArenaRoom** — PK opt-in system doesn't exist. When `cmd_attack` adds a PK-consent gate, add the `..unless ArenaRoom` branch.
- **D2. GuildhallRoom** — class trainers don't exist. When `practice` / `train` ships with per-class trainers, gate them on matching guildhall.
- **D3. NoPortalsRoom** — portal/moonwell spells don't exist. When they do, gate creation on source + destination.
- **D4. Object flag consumers** waiting for systems:
  - `PERMANENT` → needs B1 (decay tick)
  - `FLOAT` → needs air-room drop pipeline
  - `BUOYANT` → needs water surface/swim distinction
  - `VEHICLE` → mount system only supports mobs today
  - `NO_BURN` → needs fire-damage-to-inventory pipeline
  - `NO_LOCATE` → needs `locate` ability
  - `NO_INVISIBLE` → needs object-targeted invisibility
- **D5. Mob latent consumers** waiting for skills/abilities:
  - `Sized` → bash / drag / throw skills
  - `LifeForceTag` → detect_undead / holy_word / turn_undead
  - `MobTraits::Illusion` → dispel_illusion
  - `MobTraits::{PlayerPhantasm, Summoned, Pet}` → their respective game systems
  - `MovementPoints` → uncosted wander today
  - `MovementModeTag::Flying` aerial combat

## E — Improvements (not parity, may be cut)

Features that don't exist in legacy. Per user's "wire if it's an improvement" rule these were marked for eventual wiring, but they're optional.

- **E1. Object.fixture_room** — permanent objects pinned to a room (fountains, signs).
- **E2. Object.passenger_capacity** — multi-rider vehicles. Pairs with VEHICLE flag.
- **E3. Object.presence_override** — flying-carpet-style position presence override.
- **E4. Object.notes + tags** — builder search metadata. Cheap to wire (no consumer needed; just persist).
- **E5. Room.entry_restriction (Lua)** — Lua-driven entry gate. Improvement over hardcoded room flags.
- **E6. Mob.riderPresenceMessage** — mount system "X rides Y" rendering.
- **E7. Mob.activityRestrictions** — Lua-driven schedule (mob only active at night, etc.).
- **E8. CharacterItems instance state** — `condition` (item degradation), `custom_name`, `custom_examine_description`, `custom_values`, `instance_flags`, `liquid_effects`, `liquid_identified`. Player-side customization.
- **E9. Ability metadata** — `pages` (spellbook), `memorization_time`, `quest_only`, `humanoid_only`, `is_toggle`, `contested_visibility`, `visibility_check`, `notes`, `tags`, `lua_script`, `school_id`. Spellbook system + Lua hooks.
- **E10. AbilityRestrictions.custom_requirement_lua** — dynamic ability gating.
- **E11. CharacterAbilities.last_used** — cooldown integration.
- **E12. Achievement.unlocked_at** — surface in `achievements` listing.
- **E13. Trigger validation metadata** (`needs_review`, `syntax_error`) — Muditor reads these; runtime could log on trigger load.
- **E14. Shop spawn controls** — `spawn_chance`, `visibility_requirement`, `purchase_requirement` on ShopItems / ShopMobs.
- **E15. Discord bot (Muditor-side)** — consumes `PendingDiscordLinks`, posts to `DiscordConfig` channels. fierymud-rs has the hooks; the bot itself is web/Muditor work.
- **E16. wait_until minute granularity** — `MudClock` has no `minute` field; Lua `wait_until` accepts but ignores the minute arg. Add minute to `MudClock` + tick advance + `_seconds_until` math.
- **E17. ScriptVars → EntityVariables migration** — per-character ScriptVars JSON could move to the unified `EntityVariables` table. Schema enum already includes a hypothetical CHARACTER variant.
- **E18. Live playthrough verification** — the migration's test suite + world-boot prove the data flows, but no human has connected via telnet/TLS and played through a fight. Recommended before committing the migration to production. A red-team session would surface anything the static tests miss.

## F — Decisions needed (user input)

Things blocked on a design call.

- **F1. Mob.{move, hp_dice_*, damage_dice_*} long-term shape.** Today HP and per-swing base damage come from dice rolled at spawn. Combat redesign hinted at flat `max_hp` columns. Decide whether the dice approach stays (parity, but legacy-shaped) or migrates to flat ints.
- **F2. Liquid table seeded?** Catalog wires up at boot, but only 30 rows imported (legacy types). If you want more (player-craftable, magical liquids), it's a content question.
- **F3. Combat balance items in section A** — all need playtesting + design calls.

---

## Sequencing recommendation

Order if you want to maximize player-visible parity in the shortest path:

1. **A (combat balance)** — playtest the new math, tune numbers. Highest player-impact polish.
2. **B (parity wires)** — B1 (decay), B3 (aggressionFormula), B5 (level permissions) are the loudest gaps.
3. **C (flavor text)** — easy wins; one loader + one renderer per item.
4. **D items** ship as their surrounding systems ship (don't force).
5. **E** — improvements; pick whichever the player base would notice.
6. **F decisions** — answer when convenient; none are blocking.

## G — Playtest bugs (2026-05-16)

Captured during a hands-on session. Grouped by area.

### G1. UI / display
- **Combat prompt missing from `prompt list`.** Combat prompts (enemy HP bar etc.) aren't surfaced as a selectable preset.
- **`score` shows equipment slots.** At high level the slot block dominates the screen. Remove it from `score`; the standalone `slots` command already covers it.
- **`score` shows XP twice.** Dedupe. Also audit whether we have multiple score variants (compact / normal / fancy) and document which is the default.
- **`spells` should group by spell level**, not flat alphabetical. Useful for "what's my highest-circle spell".
- **`skills` should display proficiency.** Today it lists names only.
- **`eff` should auto-resolve to `effects`.** Default to prefix-matching; only ambiguity prompts on destructive commands. Apply broadly — most commands should abbreviate.

### G2. Effects + spells
- **`web` spell does not actually root the target.** Webbed character can still move freely. Spell duration is also 7576 seconds — implausibly long; needs design review.
- **No way to see what a spell does in-game.** `astat` shows the affect block but not the spell's text/effect description. Add a spell help or extend `astat` to render the spell catalog entry.
- **`help web`** (and likely most spells) returns nothing — wire spell help entries.
- **Combat-targeted spells default to the caster.** Casting `burning hands` while fighting a mob hits the caster instead of the enemy.
- **Cast output includes the full spell help block.** Should only print the spell's combat message ("Flames shoot from your fingertips…"), not the help card.
- **Burning Hands tag says "single-target / not area"** but the description says cone — declared targeting doesn't match the design. Likely a converter / catalog mismatch.
- **L21 sorc cast Burning Hands → 5661 self-damage**, instantly dying. Caster-as-target + uncapped damage = self-kill. Multiple bugs compounding (see above).

### G3. Combat flow
- **First swing delay.** `kill X` prints "You attack…" but no actual swing fires until the next combat tick. Fire one swing immediately on initiate.
- **You can keep attacking *after* death.** Dead player gets the death banner, then proceeds to `You swing at the Illithid but miss.` Need to gate the swing system on `posture != Dying|Dead`.
- **GMCP `Char.Vitals` is one round late.** Currently sent at start-of-tick (pre-damage); should send post-resolution / at the prompt.
- **Mobs appear to instantly respawn after kill.** Reset cadence is too aggressive; needs a per-mob respawn delay (legacy was ~zone repop interval).

### G4. Death / corpse / respawn
- **Corpse decay = 10 min is wrong for players.** Mob corpses fine at 10 min; player corpses should last days so the player has time to retrieve gear.
- **Release sends to The Void.** Spawn precedence should be:
  1. Save_location (only if rented/camped)
  2. Last touchstone
  3. Race home room (character-creation spawn)
  4. The Void — only if all above are missing (should never happen in practice)

  Today step 4 fires when 2 and 3 should have caught it.

### G5. Mudlet package
- **Bar overflow.** If we send `health=100/1` (max < current), the visual bar grows past 100% and obscures most of the screen. Clamp render to `min(current/max, 1.0)` and treat `max <= 0` as a sentinel "unknown — don't draw".

---

## How to use this doc

When something here ships, **delete** the bullet. This doc should always reflect "what's still pending." If something gets dropped (decided not worth doing), move it to a "Cancelled" section so the history survives.

Historical context for completed work: `migration-plan.md`, `parking-lot.md` (Resolved section), `database-audit.md`.
