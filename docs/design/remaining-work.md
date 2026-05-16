# Remaining Work

**Generated:** 2026-05-12. Last cleaned 2026-05-16. The post-migration to-do list — only items not yet shipped. Completed work lives in `migration-plan.md` (historical tracker) and `parking-lot.md` Resolved section.

Each item has:
- **Why** — parity / improvement / balance / decision
- **Scope** — rough effort, anchor file(s)
- **Blocker** if not just "needs implementation"

## A — Combat balance review

User flagged as the next session. Combat data is wired end-to-end and tests pass, but the math hasn't been balance-tuned against the design spec.

- **A1. `hit_chance_pct` magnitudes vs spec.** Current code is the proper d100 contest (`50 + (accuracy - evasion) / 2`, clamped 1..=99), but the magnitudes weren't compared against `docs/design/combat.md` targets. Run a sweep of expected hit-rates at level brackets, compare to legacy. ✅ Partially addressed by §7/§8 gear-curves sweeps + class-tier acc/eva rates.
- **A2. `attack_power` flat vs % multiplier.** Decided: AP applies as additive % multiplier (`base * (1 + AP/100)`). Implemented in `combat.rs::apply_swing` line ~813. ✅ Closed.
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
- **E18. Live playthrough verification** — periodic hands-on play remains valuable; the static tests can't surface what feels right.

## F — Decisions needed (user input)

Things blocked on a design call.

- **F1. Mob.{move, hp_dice_*, damage_dice_*} long-term shape.** Today HP and per-swing base damage come from dice rolled at spawn. Combat redesign hinted at flat `max_hp` columns. Decide whether the dice approach stays (parity, but legacy-shaped) or migrates to flat ints.
- **F2. Liquid table seeded?** Catalog wires up at boot, but only 30 rows imported (legacy types). If you want more (player-craftable, magical liquids), it's a content question.
- **F3. Combat balance items in section A** — all need playtesting + design calls.

## H — Playtest follow-ups (2026-05-16)

Open items surfaced during hands-on play. Lower priority than A-B but worth resolving.

- **H.G2.5 (Burning Hands cone vs single).** Description says "cone before you"; data has `isArea=false` and notes "touch range". Targeting now defaults to current opponent so single-target works fine. Deciding whether to upgrade to a real cone (data fix + cone implementation) vs trim the description to match the touch-attack reality is a content-author call.
- **H.1 Cleric L15 harm refused — circle-5 slot is 0.** ✅ Resolved (data + dev-mode interaction, not a bug). Cleric HARM is a circle-5 spell that unlocks at L33 per SpellSlotProgression. Adohi (L15 Cleric) does NOT know HARM in her CharacterAbilities — the earlier "refused" line surfaced only because dev-mode bypassed the KnownAbilities gate but not the slot gate. Mortal play would refuse at the knowledge gate first.
- **H.2 Cure Light at full HP shows "(heal (+0 HP))" silently.** ✅ Resolved — invoke_ability heal arm now prints "Your health is already full." (or target equivalent) and skips the no-op message.
- **H.3 Burning Hands 366 dmg vs warthog (30 HP).** Math is fine vs tier-appropriate mobs (L15 trash sits at 200-700 HP) but the one-shot feel against weakest mobs is jarring. May be acceptable flavor.
- **H.4 Rogue hide before backstab didn't appear to trigger the hidden bonus.** ✅ Resolved (data fix). BACKSTAB's `bonusIfHidden` formula was `"hidden * 0.5"` — the evaluator is integer-only outside `pow()` so it returned None and the bonus silently dropped. Rewrote as `(weapon_damage * (2 + skill / 25)) / 2 * hidden` — gives +50% damage when hidden, 0 otherwise.
- **H.5 Alignment-keyed spells silently use 1d6.** DIVINE_BOLT / DIVINE_RAY / HELL_BOLT / etc. have `amount` formulas of shape `"<expr>, then *= (caster_align * 0.001 + ...)"`. The pseudo-code `, then *=` syntax was never implemented in the evaluator. These spells fall through to the default `1d6` from spec.default_params. Needs either a multi-step formula evaluator extension OR rewritten formulas that fold the alignment term inline. Defer until the alignment combat-pipeline step lands.

---

## Sequencing recommendation

Order if you want to maximize player-visible parity in the shortest path:

1. **A (combat balance)** — remaining A4/A5/A6/A7. Highest player-impact polish.
2. **B (parity wires)** — B1 (decay), B3 (aggressionFormula), B5 (level permissions) are the loudest gaps.
3. **H follow-ups** — quick wins from the last playtest.
4. **C (flavor text)** — easy wins; one loader + one renderer per item.
5. **D items** ship as their surrounding systems ship (don't force).
6. **E** — improvements; pick whichever the player base would notice.
7. **F decisions** — answer when convenient; none are blocking.

## How to use this doc

When something here ships, **delete** the bullet. This doc should always reflect "what's still pending." If something gets dropped (decided not worth doing), move it to a "Cancelled" section so the history survives.

Historical context for completed work: `migration-plan.md`, `parking-lot.md` (Resolved section), `database-audit.md`.
