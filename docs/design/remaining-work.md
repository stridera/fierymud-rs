# Remaining Work

**Generated:** 2026-05-12. Last cleaned 2026-05-16. The post-migration to-do list — only items not yet shipped. Completed work lives in `migration-plan.md` (historical tracker) and `parking-lot.md` Resolved section.

Each item has:
- **Why** — parity / improvement / balance / decision
- **Scope** — rough effort, anchor file(s)
- **Blocker** if not just "needs implementation"

## A — Combat balance review

User flagged as the next session. Combat data is wired end-to-end and tests pass, but the math hasn't been balance-tuned against the design spec.

- **A1. `hit_chance_pct` magnitudes vs spec.** ✅ Closed — §7/§8 gear-curves sweeps + class-tier acc/eva rates address it.
- **A2. `attack_power` flat vs % multiplier.** ✅ Closed — additive % multiplier shipped in `combat.rs::apply_swing`.
- **A4. `posture_evasion_penalty` values (10/20/25/30).** ✅ Locked by regression test (`posture_evasion_penalty_table`). Adjust if playtest finds them tilted.
- **A5. `spell_power` consumer.** ✅ Closed — magical spell damage + heal scale by `(100 + spell_power) / 100`.
- **A6. `perception` / `concealment` consumer.** ✅ Closed — stealth opening-strike grants +acc/+dmg, softened by defender's perception; Stealth clears after the first swing.
- **A7. `resistances` per-element pipeline step.** ✅ Closed — `apply_resistance` mitigates per-element on both single-spec and multi-component damage paths.

## B — Parity-critical wires (legacy MUD feature gaps)

Features the legacy MUD had that we still need.

- **B1. Object decay tick** ✅ Closed — `item_decay_tick` decrements `ItemTimer` and destroys at zero (PERMANENT skipped).
- **B2. Bashable doors** (`RoomExit.hit_points`). Deferred — needs `ExitData.hit_points` plumbed through the loader before the bash handler can decrement.
- **B3. Mob.aggressionFormula** (Lua expression for varied aggro). Replaces hardcoded `AGGR_EVIL`/`AGGR_GOOD` etc. flags with per-mob Lua. Scope: load on Mob struct + eval at the wander/aggro tick site in `mud-server`.
- **B4. RaceSpellSlotBonus** (per-race +N slots for a circle). Loaded via a new module, folded into the spell-slot cap calculation when the slot system tracks circle pools. **Blocker:** 0 rows in DB; defer until content lands.
- **B5. LevelDefinition.permissions** ✅ Closed — on level-up, the row's permissions union into `Account.perms` with a player-facing notification.
- **B6. Object equip restrictions** ✅ Closed — `allowed_races` + `min_size` + `max_size` gated in the wear handler; surfaced on `identify` under a "Requirements" section.

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
- **E12. Achievement.unlocked_at** ✅ Closed — `CharacterAchievements.unlocked` is now `HashMap<i32, DateTime<Utc>>`; `achievements` listing renders `(unlocked YYYY-MM-DD)` next to unlocked rows. Virtual sessions also hydrate achievements so admin playtests see the same state real-telnet logins do.
- **E13. Trigger validation metadata** ✅ Closed — `TriggerRow` reads `needs_review` + `syntax_error`; loader emits a `warn` for stored syntax errors (fire-time failure expected) and an `info` for needs-review (builder hint). Both boot and the admin reload path go through the same helper. Verified live: 112 needs_review hits logged on boot.
- **E14. Shop spawn controls** — `spawn_chance`, `visibility_requirement`, `purchase_requirement` on ShopItems / ShopMobs.
- **E15. Discord bot (Muditor-side)** — consumes `PendingDiscordLinks`, posts to `DiscordConfig` channels. fierymud-rs has the hooks; the bot itself is web/Muditor work.
- **E16. wait_until minute granularity** ✅ Closed — `MudClock.minute` derived from within-hour tick position (12.5 ticks ≈ 1 game minute). `time.minute` exposed to Lua; `_seconds_until` honors the minute arg (5/4 real seconds per game minute). `time` command and world_status JSON render HH:MM.
- **E17. ScriptVars → EntityVariables migration** — per-character ScriptVars JSON could move to the unified `EntityVariables` table. Schema enum already includes a hypothetical CHARACTER variant.
- **E18. Live playthrough verification** — periodic hands-on play remains valuable; the static tests can't surface what feels right.

## F — Decisions needed (user input)

Things blocked on a design call.

- **F1. Mob.{move, hp_dice_*, damage_dice_*} long-term shape.** Today HP and per-swing base damage come from dice rolled at spawn. Combat redesign hinted at flat `max_hp` columns. Decide whether the dice approach stays (parity, but legacy-shaped) or migrates to flat ints.
- **F2. Liquid table seeded?** Catalog wires up at boot, but only 30 rows imported (legacy types). If you want more (player-craftable, magical liquids), it's a content question.
- **F3. Combat balance items in section A** — all need playtesting + design calls.

## I — Playtest pass 2 (2026-05-16, post-A5/A6/A7/B1/B5)

Live verification confirmed:

- **A6 stealth bonus** fires correctly. `hide` then `kill X` drops
  the player's attack DC from 78 → 65 (the +25 acc bonus minus
  defender's 0 perception/4 ≈ +13 hit% delta). Score showed the
  marker cleared after the first swing. ✓
- **A5 burning hands** now lands clean damage (335 dmg vs L15
  stallion's ~237 HP) — was 5661 before the 0..=1000 → 0..=100
  skill normalization. ✓
- **B5** unit-tested via the existing combat suite. Live test
  defers until a player actually levels up in play.
- **B1** unit-coverage; live verification deferred until a
  proto with `timer > 0` spawns. No timed protos in the seeded
  trash mobs we've used so far.

Follow-ups surfaced this pass:

- **I.1** `consider` and `score` help text still mentioned
  legacy "hit/damage roll, AC" — refreshed.
- **I.2** Cast descriptor box (gated on dev mode) still says
  "single-target / not area" for Burning Hands. Schema's
  isArea=false is the source of truth; the description
  ("scorching everything in a cone") is the disagreement. Same
  decision as G2.5 — content-author call, deferred.

## H — Playtest follow-ups (2026-05-16)

Open items surfaced during hands-on play. Lower priority than A-B but worth resolving.

- **H.G2.5 (Burning Hands cone vs single).** Description says "cone before you"; data has `isArea=false` and notes "touch range". Targeting now defaults to current opponent so single-target works fine. Deciding whether to upgrade to a real cone (data fix + cone implementation) vs trim the description to match the touch-attack reality is a content-author call.
- **H.1 Cleric L15 harm refused — circle-5 slot is 0.** ✅ Resolved (data + dev-mode interaction, not a bug). Cleric HARM is a circle-5 spell that unlocks at L33 per SpellSlotProgression. Adohi (L15 Cleric) does NOT know HARM in her CharacterAbilities — the earlier "refused" line surfaced only because dev-mode bypassed the KnownAbilities gate but not the slot gate. Mortal play would refuse at the knowledge gate first.
- **H.2 Cure Light at full HP shows "(heal (+0 HP))" silently.** ✅ Resolved — invoke_ability heal arm now prints "Your health is already full." (or target equivalent) and skips the no-op message.
- **H.3 Burning Hands 366 dmg vs warthog (30 HP).** Math is fine vs tier-appropriate mobs (L15 trash sits at 200-700 HP) but the one-shot feel against weakest mobs is jarring. May be acceptable flavor.
- **H.4 Rogue hide before backstab didn't appear to trigger the hidden bonus.** ✅ Resolved (data fix). BACKSTAB's `bonusIfHidden` formula was `"hidden * 0.5"` — the evaluator is integer-only outside `pow()` so it returned None and the bonus silently dropped. Rewrote as `(weapon_damage * (2 + skill / 25)) / 2 * hidden` — gives +50% damage when hidden, 0 otherwise.
- **H.5 Alignment-keyed spells silently use 1d6.** ✅ Resolved. Added `caster_align` / `victim_align` to FormulaCtx (engine commit 411f1ab) and rewrote all seven `, then *= ...` formulas in integer math (fierylib commit e38f2c2). Multipliers that go negative for wrong-alignment casters round to 0 amount, which the apply path skips silently — exactly the gate the original formulas intended.
  - **2026-05-16 update:** the broken formulas (DIVINE_BOLT, DIVINE_RAY, HELL_BOLT, EXORCISM, LESSER_EXORCISM, HELLFIRE_BRIMSTONE, STYGIAN_ERUPTION, FLAMESTRIKE, COLOR_SPRAY, MOONBEAM, ENERGY_DRAIN, SEED_OF_DESTRUCTION, VAMPIRIC_BREATH) were rewritten in `fierylib/data/abilities.json` to use the modern tier ladder (`NdM + pow(skill, K)`) and now resolve correctly through the evaluator instead of falling through to `1d6`. Original legacy intent preserved in each ability's `notes` field. The engine asks needed to fully re-faithfulise these spells are in section I below.

---

## I — Ability formula engine extensions (2026-05-16)

Content-side audit of `fierylib/data/abilities.json` repaired 13 damage
formulas that contained syntax the current evaluator rejects (embedded
English `, then *= ...`, `random_number(...)` typo, unknown symbols
`caster_align` / `victim_align` / `caster_INT` / `max_hp` / `level_drain`).
Each rewritten formula drops the unsupported terms and substitutes a
circle-appropriate tier formula. The original legacy intent is preserved
in each ability's `notes` field so this work can be re-faithfulised once
the engine grows the required surface.

Pick up the items in this section in roughly the order listed — the
symbols (I1) unblock the most reverts, the grammar work (I2) lets you
fold in the dynamic-exponent legacy scaling.

- **I1. Add caster/target symbols to `FormulaCtx`** (`mud-server/src/commands.rs::FormulaCtx`).
  - `caster_align` — caster `Alignment.alignment` value, -1000..1000. Read
    via `world.get::<Alignment>(player).map(|a| a.alignment).unwrap_or(0)`
    at the invoke site in `invoke_ability` (already has the lookup
    plumbing alongside `caster_stats`).
  - `victim_align` — same column on the resolved target entity.
    `world.get::<Alignment>(target).map(|a| a.alignment).unwrap_or(0)`.
  - `target_max_hp` — target's `Health.max`.
  - `target_min_level` — for spells that mention a target threshold
    (Exorcism's "skill - GET_LEVEL(victim) > 30" branch).
    Read `world.get::<Profile>(target).map(|p| p.level).unwrap_or(1)`.
  - `caster_int_raw` / `caster_wis_raw` — raw `CoreStats.intelligence`
    / `.wisdom` (3..=25 D&D-style) so spells can scale on raw score
    instead of the bonus we already expose as `int_bonus` / `wis_bonus`.
    Legacy Flamestrike used `dam *= (caster_INT * 0.007 + 0.8)`.
  - `min_level` — the spell's authored minimum level (lowest level the
    spell can be cast at across all classes). Needed for the legacy
    dynamic-exponent formula
    `exponent = 1.2 + 0.3*min_level/100 + (skill - min_level) * (0.004*min_level - 0.2) / 100`
    which keeps low-circle spells from blowing up at high skill.
    Source: take `min` over `ClassAbilities.circle` for the ability +
    a circle→level mapping; cache on the AbilityCatalog load path.

  All wire into `formula_ctx` construction around `commands.rs:10931`.

- **I2. Extend formula grammar.** Current limits in `commands.rs::evaluate_formula`:
  - **pow exponent should accept an expression**, not just a `Float` /
    `Num` literal (see `parse_factor`'s `pow` branch). Legacy
    `sorcerer_single_target` exponent is itself a function of
    `skill` and `min_level`; without expression-exponent the dynamic
    taper has to live in code rather than data.
  - **`min(a, b)`, `max(a, b)`, `clamp(v, lo, hi)`, `if(cond, a, b)`** ✅ Closed
    (2026-05-16). Legacy bonus caps like `min(sd_bonus, skill / 4)`
    and gated branches (`if(skill - 94, bonus, 0)` for "fire only at
    skill ≥ 95") now express inline. `if` is non-zero-truthy and both
    arms always evaluate (no short-circuit needed in an integer-arith
    grammar). Inverted clamp bounds silently return None so a bad
    formula falls through rather than crashing.
  - **Allow float literals outside `pow`** so multipliers like
    `* 0.0007` survive translation. Today only the `pow` exponent
    slot accepts floats; the rest of the grammar is integer-only,
    forcing every multiplier to be encoded as `* 7 / 10000`.

- **I3. Per-target multiplicative conditions in ability data.** The
  divine/unholy line (`DIVINE_BOLT`, `DIVINE_RAY`, `HELL_BOLT`,
  `HELLFIRE_BRIMSTONE`, `LESSER_EXORCISM`, `EXORCISM`,
  `STYGIAN_ERUPTION`) all scale by an alignment multiplier *after* the
  base damage roll. Even with I1 + I2 the cleanest representation is
  probably a separate `params.multipliers` list in `AbilityEffect`
  that the runtime walks post-roll, rather than baking the multiplier
  into the damage formula. Suggested shape:
  ```json
  "multipliers": [
    { "expr": "(victim_align * -7 + 8000) / 10000", "min": 0.1, "max": 1.5 }
  ]
  ```
  Bounded multipliers also cleanly express class affinity
  (Priest +25% on `Destroy Undead`, Cryomancer +25% on `Ice Storm`,
  etc.) which currently live as hardcoded class-specific clauses in
  `magic.cpp` and would otherwise pollute the damage formula.

- **I4. `xp_drain` / `level_drain` effect type.** `Energy Drain` is
  authored as a damage spell but legacy primary effect is "victim
  loses up to 40,000 XP on save fail, caster gains a quarter". JSON
  currently substitutes a small necrotic damage roll (`4d12 + pow(skill, 1.20)`)
  and notes the drain ask. To re-faithfulise: add an `xp_drain`
  (preferred) or `level_drain` (legacy-named) effect type to
  `mud-world/src/components.rs` + `EffectCatalog`, handle in
  `invoke_ability`. Pair with the `lifesteal` flag on Vampiric Breath,
  which still routes through `damage` + post-damage `heal`.

- **I5. `target_max_hp`-keyed damage for percent-HP attacks.** Monk
  chant `SEED_OF_DESTRUCTION` was authored as `max_hp * 0.05` per
  tick. With I1's `target_max_hp` symbol the formula reverts to
  `target_max_hp / 20` per tick. Currently slotted as a flat
  level-scaled formula until then.

- **I6. Damage curve coherence sweep.** Independent of the formula
  engine, the existing per-spell damage at modern (0..=100) skill
  scale is broadly under-tuned for circles 7+ because the legacy
  `pow(skill, 2) / Y` shapes were authored for the 0..=1000 skill
  range. Examples at skill=100:
  - C8 `Chain Lightning` / `Circle of Death` — 140 dmg.
  - C11 `Creeping Doom` / `Degeneration` — 100 dmg (literally `skill`).
  - C7 `Call Lightning` — 175 dmg.
  All below C1 `Burning Hands` (356 dmg). After I1+I2 land, sweep
  the catalog and convert `pow(skill, 2) / Y` shapes either to the
  modern tier ladder (`NdM + pow(skill, K)`, K by circle) or to the
  legacy dynamic-exponent formula. The 13 spells repaired in this
  pass were slotted into the tier ladder as a first pass; everything
  else still uses the under-tuned legacy shape.

  Per-spell notes describing the original (pre-conversion) legacy
  formula live in each ability's `notes` field in `abilities.json`
  so a future pass has the source intent.

- **I7. Burning Hands description was rewritten** to match the
  touch-range reality (closing G2.5 / G2.6 from the playtest list).
  If the upgrade path is "give Burning Hands a real cone target"
  instead, expect to flip `isArea` true + add `AbilityTargeting.scope =
  ROOM_ENEMIES` and walk the description back to a cone. The
  breath-spell descriptions (`ACID_BREATH`, `FIRE_BREATH`,
  `FROST_BREATH`, `GAS_BREATH`, `LIGHTNING_BREATH`, `VAMPIRIC_BREATH`)
  and `CONE_OF_COLD` were rewritten the same direction (single-target
  flavor) — flip those too if the cone path is preferred.

- **I8. `HELLFIRE_BRIMSTONE` was flipped to `isArea: true`** in this
  pass to match its description (it's the area sibling of
  `STYGIAN_ERUPTION`). Add an `AbilityTargeting.scope = ROOM_ENEMIES`
  row on import so the dispatcher reads "ROOM_ENEMIES" through the
  `valid_targets` path instead of falling back to the `def.violent`
  no-target gate.

---

## Sequencing recommendation

Order if you want to maximize player-visible parity in the shortest path:

1. **A (combat balance)** — A1/A2/A4/A5/A6/A7 all shipped. Watch for live-play tuning surprises.
2. **B (parity wires)** — remaining B2 (bashable doors, needs `ExitData.hit_points` plumb), B3 (aggressionFormula). B1/B5/B6 shipped.
3. **H follow-ups** — quick wins from the last playtest.
4. **C (flavor text)** — easy wins; one loader + one renderer per item.
5. **D items** ship as their surrounding systems ship (don't force).
6. **E** — improvements; pick whichever the player base would notice.
7. **F decisions** — answer when convenient; none are blocking.

## How to use this doc

When something here ships, **delete** the bullet. This doc should always reflect "what's still pending." If something gets dropped (decided not worth doing), move it to a "Cancelled" section so the history survives.

Historical context for completed work: `migration-plan.md`, `parking-lot.md` (Resolved section), `database-audit.md`.
