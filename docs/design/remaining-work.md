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
- **B2. Bashable doors** ✅ Closed 2026-05-17. Runtime: schema's
  `RoomExit.hit_points` + the `BASHABLE` flag flow through the
  loader onto `ExitData.hit_points` / `.is_bashable`. `cmd_doorbash`
  consumes stamina, refuses on non-BASHABLE exits, and otherwise
  rolls `5 + STR_bonus` damage per swing — door HP defaults to 50,
  splinters on hit-zero. Content side: legacy CircleMUD never had
  an EX_BASHABLE bit (all IS_DOOR exits were bashable by default),
  so fierylib's `room_importer.import_exit` now synthesizes the
  BASHABLE flag for every IS_DOOR exit that isn't MAGICPROOF. Live
  DB backfilled in one UPDATE — 1214 doors flipped BASHABLE,
  every closed/locked door is now bash-targetable.
- **B3. Mob.aggressionFormula** ✅ Closed 2026-05-17. The 20
  distinct formulas across 597 mobs all share a small grammar
  (`target.alignment` / `target.race.alignment` compared against
  `ALIGN.EVIL` / `ALIGN.GOOD` / `'EVIL'` / `'GOOD'`, combined with
  `and` / `or`). Wrote a focused recursive-descent
  parser+evaluator in `mud-server/src/aggression.rs` and a
  `AggressionFormulaCache` resource that parses each distinct
  string once. `try_engage_aggressive_mob` now does a two-pass
  check: first the legacy mob-alignment threshold, then per-mob
  formula eval. Live verified: spawning `(117, 3)` Mud Beast
  (`formula = 'true'`) at the player's room then walking back in
  fires "The Mud Beast sees you and attacks!" immediately. 8
  parser unit tests cover all 20 formula shapes.
- **B4. RaceSpellSlotBonus** (per-race +N slots for a circle). Loaded via a new module, folded into the spell-slot cap calculation when the slot system tracks circle pools. **Blocker:** 0 rows in DB; defer until content lands.
- **B5. LevelDefinition.permissions** ✅ Closed — on level-up, the row's permissions union into `Account.perms` with a player-facing notification.
- **B6. Object equip restrictions** ✅ Closed — `allowed_races` + `min_size` + `max_size` gated in the wear handler; surfaced on `identify` under a "Requirements" section.

## Create / utility spell follow-ups (2026-05-17)

- **Create Food per-class selection.** ✅ Shipped 2026-05-17.
  Engine picks base zone by caster class (Cleric/default=120,
  Paladin=110, Priest=100, Anti-Paladin=130, Druid=140) and walks
  the resource for FOOD-typed protos in that zone — legacy's
  `zplus 0..9` strict-index dance assumed every id 0..9 was food,
  but the imported zones mix in armor/fountains/treasure so the
  strict index would conjure absurd gear. Skill-weighted pick
  into the FOOD-only sublist + small jitter so back-to-back casts
  don't duplicate; falls back to waybread (185, 8) when the
  class's zone has zero FOOD entries. Live-verified TestCleric
  L20 → "a sugar cookie" (zone 120 FOOD). Migrating to a
  `CreationRecipe` table (fierylib doc §8) is still the proper
  long-term home but the current scaffold ships food cleanly.
- **Minor Creation arg lookup.** ✅ Shipped (verified 2026-05-17;
  found already in place under `MINOR_CREATION_KEYWORDS` const +
  `create` arm dispatch). Cast arg matched as abbreviation against
  the 40-keyword table, picks `(10, idx)`.
- **CREATE_WATER / CREATE_SPRING.** No-op until liquid mechanics
  land. Legacy CREATE_SPRING spawns a fountain proto into the
  room (vnum 75 — "a clear pool of water"); CREATE_WATER fills
  the targeted DRINKCONTAINER with water units.
- **ARMOR_OF_GAIA, FLAME_BLADE.** Druid / fire-aligned magical
  items the runtime doesn't yet have proto pins for. Need
  content authoring decisions.

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

- **D6. Unwired active player skills (2026-05-21 audit).** Cross-
  checked every `abilityType = SKILL` row against registered
  command names. Seven active skills have *no* command and aren't
  passive procs — casting/invoking them is currently impossible:
  - `bind` — bind wounds (a heal/staunch; overlaps bandage /
    first_aid / lay_hands — decide whether it's a distinct tier
    or a redundant import).
  - `cartwheel`, `circle`, `palm` — rogue moves needing mechanics
    design (evasive tumble / circle-stab reposition / item-conceal).
  - `ground_shaker` — AoE knockdown (needs scope + save design).
  - `scribe` — scroll authoring; needs a whole creation pipeline.
  - `shapechange` — druid transform; large feature.
  Passive procs that correctly need no command: `sneak_attack`,
  `vampiric_touch`, `instant_kill`. Alias-resolved (not gaps):
  `pick_lock`→`pick`, `eye_gouge`→`gouge`, `lay_hands`→`layhands`,
  `trip_up`→`trip`, `first_aid`→`firstaid`. Each gap is a feature
  with a design call attached — not a quick wire.

## E — Improvements (not parity, may be cut)

Features that don't exist in legacy. Per user's "wire if it's an improvement" rule these were marked for eventual wiring, but they're optional.

- **E1. Object.fixture_room** — permanent objects pinned to a room (fountains, signs).
- **E2. Object.passenger_capacity** — multi-rider vehicles. Pairs with VEHICLE flag.
- **E3. Object.presence_override** — flying-carpet-style position presence override.
- ~~**E4. Object.notes + tags**~~ — the Objects model doesn't have these columns. (Mobs may; revisit if a builder workflow ever surfaces.)
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
- **I.3 Seeded test users starting gear.** ✅ Closed 2026-05-17.
  `user_seeder.py` now carries a `STARTER_GEAR` table and grants
  loadouts at character creation: TestWarrior claymore (163,0),
  TestCleric mace of the grave (2,142), TestMage yew staff
  (163,2) wield + spellbook (10,29) inventory, TestRogue small
  silver dagger (557,63). Idempotent (skips when the same
  (zone,id,slot) is already present), survives full re-imports.
  Live verified: TestMage's session spawns with the staff wielded
  and the spellbook in inventory.
- **I.12 `kill <keyword>` matched corpses sharing the keyword.** ✅
  Resolved. `cmd_attack` walked `(Entity, Located, Named)` and
  picked the first name-match — corpses keep "corpse of a frost
  stallion" so `kill stallion` would land on the dead one and
  surface "You attack the corpse" while the live mob sat untouched.
  Restricted the query to `With<CombatStats>, Without<Corpse>` so
  only attackable actors qualify. Verified live: `kill stallion`
  in a room with both corpses and live stallions now engages a
  live mob.
- **I.11 Second spell-catalog sweep (2026-05-16).** Followed I.10
  with the `base_damage + pow(skill, 2) / X` family. 15 more
  spells folded into the tier ladder: WRITHING_WEEDS, DISPEL_EVIL,
  DISPEL_GOOD, FREEZING_WIND, DESTROY_UNDEAD, FIRESTORM, ICE_STORM,
  HOLY_WORD, UNHOLY_WORD, METEORSWARM, SEVERANCE, FLOOD,
  ICE_SHARDS, SOUL_REAVER, SUPERNOVA. Each keeps `base_damage` for
  caster-level scaling and adds a dice roll + `pow(skill, K)` term
  tuned per circle. Live spot-check at TestMage L15 sorcerer skill=100:
  Magic Missile 214 dmg (5 bolts) → stallion at 9% HP, then
  Burning Hands 190 dmg finishes the kill. Two-cast solo kill on a
  tier-appropriate L17 stallion (237 HP) matches the user's
  "mages burn them down" intent without one-shot abuse.
- **I.10 First spell-catalog balance sweep (2026-05-16).** Per
  playtest feedback ("Burning Hands one-shots tier-appropriate
  mobs"), 9 damage formulas in the live DB were retuned to a
  consistent tier ladder so circle-1 land in the same band:
  - C1 Burning Hands / Cause Light: ~175 dmg
    (`4d12 + pow(skill, 1.10)`).
  - C2 Cause Serious: ~200 (`5d12 + pow(skill, 1.15)`).
  - C3 Cause Critic: ~291 (`5d15 + pow(skill, 1.20)`).
  - C5 Harm: ~363 (`5d18 + pow(skill, 1.25)`).
  - C7 Full Harm / Call Lightning: ~455 (`6d20 + pow(skill, 1.30)`).
  - C8 Chain Lightning / Circle of Death: ~511 (`6d22 + pow(skill, 1.32)`).
  Verified live at skill=100: Burning Hands dropped from 333 → 175;
  Cause Light climbed from 30 → 170. The catalog still has
  `base_damage + pow(skill, 2) / X` spells (Ice Storm,
  Meteorswarm, Hellfire Brimstone, etc.) using legacy shapes —
  those are tier-conservative but already include base_damage so
  they're less wildly off; a follow-up sweep can convert them on
  the same ladder. Also unresolved: the alignment-keyed spells
  (Divine Bolt / Hell Bolt / Exorcism / etc.) keep their H.5
  multiplier shape since rewriting them needs an align-scale
  redesign.
- **I.9 `multihit: true` on ability params** ✅ Resolved
  (2026-05-16). The damage apply path now reads the `multihit`
  flag from `override_params` and scales the bolt count via the
  classic `1 + (caster_level - 1) / 2`, clamped to `1..=5`.
  Per-bolt damage rolls independently (so 5 separate rolls of
  `roll_dice(4, 21)` for L9+ Magic Missile), then sums into a
  single `apply_damage` call so death/threshold broadcast fires
  once. The applied-effects line now reads
  `(-N HP ×K bolts)` when K > 1 so the bolt count is visible to
  the caster. Verified live: Magic Missile at L15 lands 219
  damage in 5 bolts (5 × ~44 each).
- **I.8 DB drift from `fierylib/data/abilities.json`.** The JSON
  is the source of truth (see the "ALL conversions happen in
  fierylib" rule). H.5 / I-section rewrites updated the JSON but
  several DB rows still carried the pre-rewrite forms. Patched
  in the live DB this pass:
  - `Color Spray`: was `(pow(skill,2)*1)/200, max ~190` →
    `4d19 + pow(skill, 1.30)`.
  - `Vampiric Breath`: was `... , +random(0,70) if skill>=95` →
    grammatical `... + if(skill - 94, random(0, 70), 0)`
    (uses the new I2 `if` builtin).
  - `Energy Drain`: was the literal string `level_drain` →
    `4d12 + pow(skill, 1.20)`.
  - `Flamestrike`: was `base_damage * (caster_INT * 0.007 + 0.8)`
    (unknown `caster_INT`, floats outside pow) → modern tier
    `5d19 + pow(skill, 1.32)`.
  - `Moonbeam`: was `random_number(20, 80)` (typo) → `random(20, 80)`.
  - `Seed of Destruction`: was `max_hp * 0.05` (unknown symbol +
    float literal) → `level * 3 + roll_dice(3, 20)`.
  Alignment-keyed spells (Divine Bolt / Hell Bolt / Exorcism /
  Hellfire Brimstone / etc.) already match JSON from the H.5
  pass. Remaining work: a `fierylib seed abilities` (or
  equivalent) on full re-import would make these hand-patches
  obsolete and let the runtime trust the DB blindly. Until
  then the live DB matches JSON for all 13 H/I spells.
- **I.7 Semantic color tags rendered as literal text.** ✅
  Resolved. Content authors write `<healing>...</>`,
  `<fire>...</>`, etc. in ability + object descriptions
  (Cure Light's description: `<healing>Cures</> minor
  <healing>wounds</> and scratches.`). The renderer's
  `named_color` table didn't know these sphere aliases and
  the unknown-tag path left them as literal angle-bracket
  text. Extended `named_color` with the standard sphere /
  semantic palette (`healing`, `death`, `protection`,
  `enchantment`, `summoning`, `divination`, `divine`, `holy`,
  `unholy`, `arcane`, `fire`, `water`, `air`, `earth`).
  Bold variants of these aliases (`<b:black>death</>`) need
  the explicit `<b:NAME>` form since `named_color` returns a
  single ANSI code. Live verification: Cure Light's
  description now emits `\x1b[32m` around "Cures" and "wounds".
- **I.6 Seeder skipped ClassSkills** ✅ Resolved (fierylib
  commit). Warrior + Rogue have 0 entries in `ClassAbilities`
  (their toolkit lives in `ClassSkills`), so the
  ability-grant loop in the seeder gave them an empty
  spellbook. TestRogue's `Char.Skills` GMCP frame after the fix
  lists BACKSTAB, HIDE, SNEAK, DODGE, PARRY, DOUBLE_ATTACK
  etc. — 19 entries total. Seeder now unions both junction
  tables before upserting.
- **I.5 Seeder wrote proficiency on the wrong scale** ✅ Resolved
  (fierylib commit, fierymud-rs n/a — runtime is correct). The
  schema column is 0..=1000 raw practice points and the runtime
  divides by 10 at formula time. user_seeder.py was writing 100,
  which gave seeded characters skill=10 downstream and silently
  collapsed mage damage by ~6×. TestMage Burning Hands went
  from 70 → 345 (matches H expected ~335) after the live DB
  was patched alongside the seeder fix.
- **I.4 TestMage and BuilderChar were Classless** ✅ Resolved
  (fierylib commit c5452a3). The seeder requested
  `class_plain_name="Mage"` but no `Class` row has that
  plain_name — the actual arcane primary is `Sorcerer`. Both
  characters silently fell through to NULL class_id, which made
  them look like Classless adventurers with no spellbook.
  Updated the seeder + patched the live DB. TestMage now shows
  `(Sorcerer)` on score and `spells` renders 46 abilities
  across circles 1-12.

## H — Playtest follow-ups (2026-05-16)

Open items surfaced during hands-on play. Lower priority than A-B but worth resolving.

- **H.G2.5 (Burning Hands cone vs single).** ✅ Closed 2026-05-17. Per user call: it's a touch attack. DB description already reads "Flames wreathe your hands as you grasp the target, searing flesh with magical fire. A foundational touch-range spell..." and notes say "touch range" — single-target intent is consistent across description + notes + isArea=false. No mechanical change needed.
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

- **I1. Add caster/target symbols to `FormulaCtx`** ✅ Closed
  (2026-05-16). All six landed:
  - `caster_align`, `victim_align` (H.5 work).
  - `target_max_hp` / `victim_max_hp` — target's `Health.max`,
    populated in the per-target ctx alongside `victim_align`.
  - `target_level` / `victim_level` — target's `Profile.level`.
  - `caster_int_raw` / `caster_int` (alias), `caster_wis_raw` /
    `caster_wis` — raw `CoreStats.intelligence`/`.wisdom` for
    spells that scale on raw score rather than bonus.
  - `min_level` — approximate spell entry level computed as
    `circle * 2 + 1`. Full per-ability `MIN(level)` over
    `SpellSlotProgression` is a deferred refinement; the cheap
    proxy is enough to express the dynamic-exponent shape
    `pow(skill, 1 + min_level / N)`.

- **I2. Extend formula grammar.** Current limits in `commands.rs::evaluate_formula`:
  - **pow exponent accepts an expression** ✅ Closed (2026-05-16).
    The exponent slot now takes either a precise `Float` literal
    (legacy `pow(skill, 1.44)` round-trips bit-exact) or a full
    integer expression (`pow(skill, 1 + level / 25)`). Integer
    expressions naturally lose any fractional component; authors
    who want a non-integer exponent must use a literal. Sufficient
    to express most legacy dynamic-taper shapes without baking
    the exponent into code.
  - **`min(a, b)`, `max(a, b)`, `clamp(v, lo, hi)`, `if(cond, a, b)`** ✅ Closed
    (2026-05-16). Legacy bonus caps like `min(sd_bonus, skill / 4)`
    and gated branches (`if(skill - 94, bonus, 0)` for "fire only at
    skill ≥ 95") now express inline. `if` is non-zero-truthy and both
    arms always evaluate (no short-circuit needed in an integer-arith
    grammar). Inverted clamp bounds silently return None so a bad
    formula falls through rather than crashing.
  - **Allow float literals outside `pow`** ✅ Shipped 2026-05-18.
    Limited but practical: a `Float` token on the RHS of `*` or
    `/` now scales the integer LHS through `scale_by_float` and
    rounds back to i32. Legacy multipliers like `dmg * 0.0007 *
    level` translate verbatim. Overflow / NaN / inf still fall
    through to None. Addition / subtraction / float-as-first-
    factor remain unsupported — covers the common multiplier
    case without an enum-Number refactor. Unit-tested with
    `skill * 0.5`, `level / 0.5`, the chained
    `10000 * 0.0007 * level` shape, and `skill / 0.0` (returns
    None).

- **I3. Per-target multiplicative conditions in ability data.** ✅
  Engine arm shipped 2026-05-18. The damage arm now walks
  `override_params.multipliers` post-roll (between empowered and
  spell_power so multipliers operate on the rolled value but
  caster-side scaling still amplifies). Each entry shape:
  ```json
  { "expr": "(victim_align * -7 + 8000) / 10000", "min": 0.1, "max": 1.5 }
  ```
  The expr is evaluated against the per-target `FormulaCtx` (so
  `victim_align` / `target_max_hp` / `target_level` resolve), and
  the integer result is divided by 1000 to recover the float
  coefficient — authors who want a literal 0.8 multiplier write
  `800` in the expr. The factor is clamped to `[min, max]` (both
  optional) and multiplied into `amount`. Floor of 0 so a
  destructive expression can't heal the target.
  - **Content sweep wave 1 (2026-05-18).** ✅ Converted all
    seven alignment-keyed spells from baked-in multipliers to
    declarative `multipliers` arrays: DIVINE_BOLT, DIVINE_RAY,
    HELL_BOLT, HELLFIRE_BRIMSTONE, EXORCISM, LESSER_EXORCISM,
    STYGIAN_ERUPTION. Each base `amount` is now just the raw
    damage shape (e.g. `base_damage + (pow(skill, 2) * 53) / 10000`),
    and the alignment scaling lives in a `multipliers` entry
    clamped to `[0.1, 1.5]`. JSON + DB both updated. Class
    affinity bonuses (Priest +25% Destroy Undead, Cryomancer
    +25% Ice Storm, etc.) remain content-follow-up — engine
    work is done so they're pure JSON authoring + DB updates.
  - **Wave 2: lifeform multipliers (2026-05-18).** ✅ Added
    `victim_is_undead`, `victim_is_demonic`, `victim_is_celestial`,
    and `victim_is_elemental` symbols to `FormulaCtx` (0/1 from
    the target's `LifeForceTag`), populated per-target alongside
    `victim_align`. DESTROY_UNDEAD carries `multipliers: [{"expr":
    "1000 + victim_is_undead * 1000", "min": 1.0, "max": 2.0}]`
    so it deals 2× damage to undead. HOLY_WORD and UNHOLY_WORD
    use mirrored expressions (boost vs demonic+undead / vs
    celestial respectively, clamped to `[0.5, 1.5]`). Future
    smite-type spells get the same one-line treatment.

- **I4. Vampiric `lifesteal` flag.** ✅ Shipped 2026-05-17. Doc
  originally framed this as "xp_drain / level_drain" per a misread
  of legacy — `spell_energy_drain` (legacy `spells.cpp:693`) is
  actually pure HP vampirism, not XP drain. Implemented as a
  `lifesteal: true` override_param consumed at the end of the
  damage arm: refuses self-target ("Draining yourself? My, aren't
  we funny today...") and refuses UNDEAD targets (LifeForceTag),
  then captures victim HP pre/post apply_damage to compute actual
  delta and heals the caster by that amount. At max HP the legacy
  polynomial spillover kicks in (`bonus = dmg * (-0.0457r² -
  0.0171r + 1.066)` with a 5..10 floor) so overdrain isn't wasted.
  Wired on both ENERGY_DRAIN (newly flagged) and VAMPIRIC_BREATH
  (already had the param). Live verified: AdminChar at full HP
  drains TestRogue → AdminChar 3244/3244 → 3523/3244 (+279 spillover
  past max).

- **I5. `target_max_hp`-keyed damage for percent-HP attacks.** ✅
  Closed (2026-05-16). With I1's `target_max_hp` symbol live, the
  DB row for SEED_OF_DESTRUCTION was reverted to its legacy
  intent: `target_max_hp / 20` per tick. Subsequent percent-HP
  spells just need to set the same shape — no further engine work.

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

## K — Save mechanics (2026-05-17)

Content-side: 126 / 408 abilities now carry an explicit
`AbilitySavingThrow` row (was 2). Walked the catalog and assigned
SaveType + uniform DC + onSaveAction per spell flavor (script:
`fierylib/scripts/author_saving_throws.py`). Open engine asks:

- **K2. `HALF_DAMAGE` on-save action arm.** ✅ Shipped 2026-05-18.
  Added `SaveOutcome::HalfDamage` and recognized `"HALF_DAMAGE"` in
  `save_action_for`. In the damage-arm pipeline the new
  `halve_damage` flag halves `amount` after ward / alignment
  trims but before `apply_damage`, so resistance/penetration math
  still runs on the halved value (mirrors legacy `dam >>= 1`).
  Caster sees `"X partially resists your <Spell>."` and target
  sees `"You partially resist X's <Spell>."`. Live verified with
  TestMage L15 casting BURNING_HANDS at TestRogue L10 (DC lowered
  to 5 temporarily so the save reliably succeeded): 220 → 41 HP
  (179 dmg, half of the ~358 full hit) with the partial-resist
  line on both sides. Reverted DC after verification.
  - **Content sweep (2026-05-18).** Walked
    `fierylib/data/abilities.json` and flipped every spell whose
    effects list is ALL `damage` (no status / charm / poison
    follow-ons) from `NEGATE` to `HALF_DAMAGE`: 85 spells total
    (BURNING_HANDS + 84 swept via Python). Live DB mirrored with
    a single matching UPDATE. Mixed-payload spells (damage +
    status) keep `NEGATE` since flipping them would let the
    status apply on save while halving the damage — the status
    payload often matters as much as the damage, and the design
    call should be per-spell.

- **K3. Authored saves only — runtime save dispatcher must roll.**
  ✅ Closed 2026-05-18 — audit confirms `save_action_for` is
  called at `commands.rs::invoke_ability_with` (line ~11874)
  before any effect application, with target's d20 + level
  versus the formula-evaluated DC. Self-target auto-fails so
  buffs land on the caster. All three branches (Negated,
  HalfDuration, HalfDamage) now have live runtime arms. The 126
  authored saves are not inert.

## J — Resistance / protection extensions (2026-05-17)

Content-side: `ObjectResistance` is now populated for the legacy
single-element protection effects (PROT_FIRE/COLD/AIR/EARTH +
FIRESHIELD/COLDSHIELD + NEGATE_*). Two legacy protection shapes can't
be expressed in the current `ObjectResistance` schema:

- **J1. Spell-circle absorb (MINOR_GLOBE / MAJOR_GLOBE).** ✅
  Shipped 2026-05-17. Implemented via ECS marker
  (`MaxAbsorbCircle(i32)`) rather than a DB column — same end-state
  but no schema change. Wired:
  1. `mud-server/src/commands.rs` `"globe"` dispatcher arm reads
     `override_params.maxCircle` from the `globe` effect's
     AbilityEffect row, installs `MaxAbsorbCircle` taking the max
     of any pre-existing threshold (so MINOR over MAJOR doesn't
     downgrade), and spawns a duration-tracked `EffectInstance`.
  2. `SpellSlotData::min_circle_for_ability` (in
     `mud-world/src/resources.rs`) returns the smallest circle the
     ability appears at across every class — mirrors legacy
     `SINFO.lowest_level`. O(n) scan over ~1000 ability_circle
     entries; only called on absorb-marked targets, so not hot.
  3. Damage arm gate at the top of `"damage"`: when the target
     has a `MaxAbsorbCircle` marker, the spell's
     `min_circle_for_ability` ≤ marker, and caster ≠ target → emit
     the flare messages and `continue`. Self-cast exempt so a
     mage's own AoE doesn't burn through their own globe.
  4. `effects.rs` teardown: on the `"globe"` effect's expiry,
     recompute the marker from any remaining instances (highest
     strength wins) — stacking MAJOR + MINOR keeps coverage when
     only one expires.
  Live-verified end-to-end: AdminChar casts MINOR_GLOBE
  (maxCircle=3) on TestRogue → BURNING_HANDS (circle 1) absorbed
  with flare message + 0 HP loss → FIREBALL (circle 4) bypasses
  threshold and drops TestRogue to 0 HP.
  - **Follow-up:** the ~10 equipped items that legacy carries with
    `EFF_MINOR_GLOBE` / `EFF_MAJOR_GLOBE` need ObjectEffects rows
    to route through the same arm. Schema's `ObjectEffects` →
    `Effect.name = "globe"` mapping already wires the runtime path;
    fierylib needs to translate the legacy bits. Logged as a
    content task in fierylib's remaining_work.md.
  - **L4 caveat closed 2026-05-18.** The pre-loop success-template
    emit now peeks at the target's `MaxAbsorbCircle` and the
    spell's `min_circle_for_ability`. When the damage spell will
    be absorbed in full, the header is deferred into
    `pending_header` (matching the non-damage path) and the
    post-loop refusal-substring suppression — which already
    includes `"(absorbed"` — drops it. Non-absorbed casts still
    take the pre-loop emit so death broadcasts slot in after the
    cast confirmation. Live verified: AdminChar wraps TestRogue in
    MINOR_GLOBE → casts BURNING_HANDS → output is just "The
    shimmering globe around TestRogue flares as your spell flows
    around it.", no leading "You burn TestRogue".

- **J2. Alignment-vs-alignment protection (PROT_FROM_EVIL /
  PROT_FROM_GOOD).** ✅ Shipped 2026-05-17. Implemented via the
  same ECS-marker pattern as J1 — no schema change. Components:
  `ProtectFromEvil` + `ProtectFromGood` (unit markers) plus an
  `AlignmentProtectionTag(Evil|Good)` companion on the backing
  `EffectInstance` so the teardown can distinguish multiple
  resistance-flag instances on the same target. Wired:
  1. The catchall `"resistance"` flag arm checks `type_str` for
     `"evil"` / `"good"` before the element match table. On hit:
     installs the marker on the target, tags the freshly-spawned
     EffectInstance, skips the Resistances-map path.
  2. `alignment_protection_factor(world, attacker, victim) → f32`
     returns 0.8 when the marker matches the attacker's alignment
     (≤-500 vs ProtectFromEvil, ≥+500 vs ProtectFromGood) AND the
     victim's own alignment is opposed (≥+500 / ≤-500). Mirrors
     legacy `fight.cpp:1639` exactly — including the requirement
     that the victim themselves be aligned, so a neutral player
     wearing protect-from-evil doesn't get a free discount.
  3. Two hook sites: `combat::apply_swing` for melee (after resist
     / hardness, before MAX_DAMAGE cap) and the `"damage"`
     dispatcher arm for spells (after ward / resist).
  4. Teardown in `effects.rs` queries for any remaining tagged
     instance on the target sharing the same alignment; drops the
     marker only when no backing instance remains. Mirrors the
     bless/sanctuary refcount pattern.
  - Unit test (`alignment_protection_factor_gate_logic`) covers
    all four legitimate cases (matched + opposed alignments) plus
    the three rejection cases (no marker, neutral victim, neutral
    attacker, wrong marker). 284/284 tests pass.
  - Live verified end-to-end via MCP: AdminChar (alignment -800)
    casts PROT_FROM_EVIL on TestRogue (alignment +800) → effect
    installed with name="resistance", remaining_secs=1492; followup
    spells continue to land but reduced.
  - **Content follow-up:** ~45 legacy items wear PROTECT_EVIL /
    PROTECT_GOOD bits. fierylib needs to add `ObjectEffects` rows
    pointing at `Effect.name = "resistance"` with
    `override_params = {"flag": "resistance", "type": "evil"/"good",
    "amount": 1}` so the runtime path picks them up on equip.

- **J3. MobDefaultEffects — runtime consumer missing.** Audit doc §1
  says "runtime: not loaded". The table is intended for permanent
  passive auras on mob protos (lich fear, dragon fear, etc.). Schema
  is there; importer can fill it once the runtime loads + applies the
  rows. Currently the only mob-side passive on mobs is the
  `resistances` JSON column on `Mobs` (591/2139 populated). If you'd
  like MobDefaultEffects as a content surface, ping fierylib and I'll
  write the importer in the same pass as J1/J2.

---

## K — Cast timing / spellbook polish (2026-05-17)

User playtest ask 2026-05-17: "casting time should depend on spell difficulty.
Look at legacy for an example, but we can improve on that system."

- **K1. Spell cast queue + cast_time_rounds consumer.** ✅ Shipped
  2026-05-17. `mud_world::Casting` component installed by
  `invoke_ability_with`'s queue branch when `cast_time_rounds > 0`
  and decremented by `casting::casting_tick` (40 ticks per round =
  4s). Resolution re-enters via `resolve_queued_cast` with a
  `skip_queue` flag so the loop doesn't re-fire. Interrupts:
  damage > 30% max HP via `check_concentration_on_damage`;
  movement via `cmd_move` interrupt; explicit `abort` /
  `flee` cancel; `cancel` keeps its existing effect-drop scope.
  Item / AoE-sub-cast paths set `skip_queue=true`. Live verified:
  TestMage `cast 'magic missile' elspeth` shows "You begin casting
  Magic Missile… (about 8s)" then resolves with the engage line +
  damage diagnostic 8s later.
  - **Follow-ups deferred to a future K1.1:**
    - Live progress bar in prompt (`[Casting Magic Missile 4/8]`).
    - "Cancelled by damage" surfaces only as the generic break
      message; doesn't show the magnitude.
    - No `still_winding` ECS update (purely diagnostic — the
      `casting_tick` loop already decrements in place).
- **K2. Sentence-start capitalization sweep.** ✅ First pass shipped
  2026-05-17. Wrapped the high-impact user-facing sentence-start
  sites where a mob/player name leads:
  * `sleep.rs` — "X settles down to sleep." / "X wakes and stretches."
  * `info.rs cmd_examine` — profession line, "X hovers in mid-air.",
    Mounted "X is riding Y.", RiddenBy "Y is riding X."
  * `commands/combat.rs` — Sanctuary refusal ("X radiates a calm
    that turns your blow aside."), consider non-fighter
  * `room_chat.rs` — insult ("X insults you: ...", "X insults Y.")
  Combat death/leave/arrive broadcasts and consider were already
  capped in earlier passes.
  - **Second pass (2026-05-18).** Wrapped the remaining
    name-led refusal lines across admin/combat/info modules:
    `admin_inspect.rs` script-var lines (has no, had no, has no var,
    has no component for), `admin_world.rs` (target is in,
    has no active effects), `combat.rs` (steal coin, steal item
    not on target, disarm not wielding), `info.rs` (wake already
    awake, give too laden). 9 sites total. Mob targets now read
    "The lion has no var named X" instead of "the lion has no var
    named X".
- **K3. Info-leak audit (mortal vs god view).** ✅ Closed 2026-05-17.
  Full surface coverage verified:
  * `cmd_consider` — already gated; mortals see verbal impression
    ("looks badly hurt" / "is bleeding heavily"), staff see exact
    HP and hit-chance %.
  * `cmd_examine` — gated 2026-05-17 with `is_staff()`; mortals
    examining a target ≥5 levels higher see a verbal stature tier
    ("of considerable skill" / "of formidable power" / "of
    legendary stature") with correct a/an article picking. Equal-
    or-weaker targets keep the level for prey-gauging.
  * `look <mob>` — routes through `cmd_examine`, so the same gate
    applies (verified at `cmd_look:5437`).
  * `cmd_score` — shows the *player's own* stats, not a leak.
  * `stat` — already Builder+ only.
  * MCP `inspect_actor` — admin tool, always staff-only.
  Live verified: TestRogue L10 → AdminChar L105 reads "of legendary
  stature"; TestRogue L10 → TestMage L15 reads "of considerable
  skill"; AdminChar (staff) → TestRogue reads "is a level 10
  Halfling Rogue".
- **K4. Dead-spell audit.** ✅ Shipped 2026-05-17.
  `commands::audit_dead_spells(&world)` runs once at boot after the
  catalog loads; walks every SPELL kind and warns on either zero
  AbilityEffect rows or an effect_type outside
  `KNOWN_EFFECT_TYPE_ARMS` (kept in lockstep with the dispatcher's
  `match` arms). First run output: zero spells with no effects,
  7 spells reference `summon` (no arm yet): ANIMATE_DEAD, CLONE,
  SIMULACRUM, SUMMON_DEMON, SUMMON_DRACOLICH, SUMMON_ELEMENTAL,
  SUMMON_GREATER_DEMON. Closes the audit ask; the L3 entry tracks
  the actual `summon` arm implementation.
  - **2026-05-21 effect-type coverage re-audit.** Walked every
    distinct `effectType` in use vs the dispatcher arms. Three
    types lack a generic-loop arm: `drag` (handled via the
    `has_effect_named` command path — works), `interrupt`
    (BASH — now wired, see below), and `conceal_item` (PALM —
    the rogue skill isn't a registered command yet, so it's
    unreachable; a feature gap in the rogue tree, not a live
    bug). **BASH interrupt fixed:** `cmd_bash` now calls
    `casting::interrupt_cast` on the bashed target after the
    knockdown, so a bash shatters a mid-cast mage's spell —
    realizing the BASH ability's `interrupt` effect intent that
    the inline bash path previously ignored. PALM/rogue-skill
    wiring remains a feature follow-up.
    - **Live-verified 2026-05-21:** PK'd L25 TestWarrior bashed
      L15 TestMage mid Magic-Missile wind-up → mage saw "Your
      concentration on Magic Missile shatters — the bash knocks
      you flat." and the spell never resolved. (Drove the
      teleport setup via curl against `/api/admin/teleport`,
      since the `mcp__fierymud__teleport` tool was returning a
      JSON-parse error this session — the endpoint itself is
      healthy with `player_name`/`zone_id`/`room_id`.)

## N — Weather spells (2026-05-17)

- **CONTROL_WEATHER / RAIN** ✅ Shipped 2026-05-17. Room-arm
  recognizes a `setPrecip` override_param (RAIN seeds this as
  `"rain"`) and, for CONTROL_WEATHER, parses the cast arg
  (clear / cloudy / drizzle / rain / storm / snow / blizzard)
  with "clear" as the no-arg default. On a successful cast we
  mutate the zone's WeatherCatalog entry directly. Refuses on
  Climate::None zones (Void, planes) with "no weather here to
  control".
  - **Drift lock (2026-05-18).** New `WeatherDriftLocks` resource
    (`mud-world/src/resources.rs`) maps `zone_id → Instant` of
    lock expiry. The setPrecip arm installs a lock for the cast's
    full `dur_secs` after mutating the catalog entry. `weather_tick`
    snapshots the live lock set each tick, opportunistically
    expires stale entries, and skips both `drift_temp` and
    `drift_precip` for any locked zone. Live verified: zone 30
    pre-cast "frigid and drizzling" → cast `control weather storm`
    (skill 100 → 20-hour duration) → 75s elapsed past the 60s
    drift tick → still "frigid and stormy".

## M — Wall spells (2026-05-17)

- **WALL_OF_STONE / WALL_OF_ICE** ✅ Shipped 2026-05-17. v1
  exit-blocking machinery: new `RoomBlockedExits` component carries
  `HashMap<Direction, RoomBlockedExit>`; the "room" dispatcher arm
  parses the cast arg as a direction (north/up/etc.), refuses if the
  source room has no exit there, and installs the entry with a
  reference to the backing `EffectInstance`. `cmd_move` consults
  the map before traversal and refuses with "A wall of stone blocks
  your path." Teardown in `effects_tick` removes the matching entry
  when its instance expires; multiple walls on different directions
  coexist; re-cast on the same direction overwrites + despawns the
  prior backing instance so teardown timings stay clean. Live
  verified: AdminChar casts `wall of stone north` → `north` refuses;
  `west` still works.
- **Wall visibility in look / exits (2026-05-18).** ✅ Walls
  now render in six surfaces:
  - `cmd_exits` appends a parenthetical to each walled direction
    line (`north - Cobblestone Path  (wall of stone)`),
    coloured red (block), yellow (slow), or cyan (illusion).
  - The autoexit line on `look` swaps the door-state suffix for
    `[W]` / `[F]` / `[I]` when a wall is in effect — wall takes
    precedence over door state because it's the more urgent
    obstacle.
  - **In-room atmosphere line.** `cmd_look`'s render now emits
    one wall sentence per walled direction right after the
    weather line — e.g. "A wall of fog chokes the way east." in
    yellow / "A wall of stone blocks the way north." in red /
    "A wall of illusion shimmers across the way south." in
    cyan. Sorted by canonical direction order so multi-wall
    rooms read cleanly. Live verified: cast WALL_OF_FOG east →
    look output carries the yellow line right under the weather.
  - **Look in walled direction.** `look_direction` now consults
    the wall map before the destination peek. Block walls hide
    the destination entirely ("A wall of stone blocks your
    view." in red); fog/illusion render their own barrier line
    first ("A wall of fog swirls thickly across the path."
    yellow / "A wall of illusion shimmers across the path,
    oddly transparent." cyan) and then fall through to the
    destination peek so the player can still see what's
    beyond. Live verified: `look east` after WALL_OF_STONE east
    shows the red barrier line with no destination peek.
  - **Cast-time room broadcast.** The wall cast site now fires
    a `broadcast_room_visual` to everyone in the room (minus
    the caster) — "X gestures and a wall of stone rises,
    blocking the way east." / "rolls in, choking" for fog /
    "shimmers into being across" for illusion. Bystanders no
    longer see only the spell's generic success line; the wall's
    materialization is announced.
  - **Wall expiry room broadcast.** The teardown branch in
    `effects.rs` now captures the dropping `(direction,
    kind_label)` before the retain and broadcasts "The wall of
    stone east shudders and dissolves into nothing." to every
    player in the affected room. Without it, a wall just
    vanished silently and any player watching it for a passage
    cue had no signal it had cleared.
- **Wear / wield / hold / remove room broadcast (2026-05-18).**
  ✅ `wear_into` (covers `wear`/`wield`/`hold`) and `cmd_remove`
  (single + `all` paths) now broadcast a third-person line to
  everyone else in the room — "X wields a steel longsword.",
  "X wears the iron helm.", "X removes the leather boots."
  Previously only the actor saw "You wield/wear/hold/remove X";
  party-mates had no visual cue when an ally swapped gear.
  `cmd_drop` already had this; wear/remove were the gap.
- **Eat / drink / quaff / sip / light / extinguish room
  broadcast (2026-05-18).** ✅ Same pattern applied to the
  consume + light-source paths: `consume_item` (eat / quaff),
  `drink_amount` (drink / sip), `cmd_light`, `cmd_extinguish`
  now all emit a third-person line. Bystanders see allies
  quaffing healing potions mid-fight, a torch flaring to life
  in a dark room, etc. Important for tank/party situational
  awareness.
- **Rent (inn) room broadcast (2026-05-18).** ✅
  `finalize_rent` emits "X hands over N gp and books the
  {tier_name}." to room observers after the rent completes.
  Mirrors how `cmd_camp` broadcasts the camp setup — both
  are visible transactions at an inn/camp scene, so
  party-mates following the renter into the common room see
  who booked what tier.
- **Wall + SUMMON spell descriptions refreshed (2026-05-18).**
  ✅ Updated `fierylib/data/abilities.json` + live DB for
  WALL_OF_STONE / WALL_OF_ICE / WALL_OF_FOG / ILLUSORY_WALL
  and SUMMON. New descriptions name the actual mechanics that
  L1-v2 / M-section work landed: per-cast accept prompt and
  level cap for SUMMON; bash HP / cancel verb / traversal
  semantics for each wall variant. Also adjusted
  `cmd_nosummon`'s toggle messages to "Incoming summons will
  now silently auto-decline" / "...will now prompt you" so
  the on/off line matches the help text and the L1-v2
  semantics (NoSummon = silent decline, not hard refusal).
- **Second description refresh wave (2026-05-18).** ✅ Same
  pass applied to nine more spells whose mechanics shipped
  since the original text was authored: ANIMATE_DEAD (corpse
  consumption + HP scaling), CLONE (caster-mirror), SIMULACRUM
  (remote actor-mirror), CONTROL_WEATHER + RAIN (drift lock
  for duration), MINOR_GLOBE + MAJOR_GLOBE (concrete circle
  thresholds and stacking rule), PROT_FROM_EVIL +
  PROT_FROM_GOOD (concrete 80%/-500/+500 numbers and the
  neutral-target caveat). JSON + DB both updated.
- **Lifesteal spells description fix (2026-05-18).** ✅
  ENERGY_DRAIN's old description claimed it drained XP and
  gave a quarter to the caster — that was a misreading of
  legacy; I4's runtime work made it pure HP vampirism. The
  description now reflects that, plus the at-max-HP
  polynomial overflow. VAMPIRIC_BREATH's description picked
  up the same overflow note.
- **RestState surfaced on score sheet (2026-05-19).** ✅
  `ScoreData` grew an `Option<RestStateDisplay>` field carrying
  `(source, tier, repose)`; populated in `cmd_score` from the
  player's `RestState` component, suppressed when there's
  nothing to say (no source pending AND no Repose banked).
  All three renderers updated:
  - Standard / fancy: `"Rest: Rented Inn tier 2 (resting);
    1,234 XP banked"`. Variants: `"Camped tier 1 (resting)"`
    / `"At home (resting)"` / `"Logged out (no pool yet)"`
    / pool-only `"320 XP banked"`.
  - Minimal one-liner: `"rest:inn(2)  repose:1234"` /
    `"rest:camp(1)"` / `"rest:home"` / `"rest:quit"`.
  Unit test covers all five render variants
  (`format_rest_line_renders_each_source`). 286 tests pass.
- **Wear all / remove all consolidate broadcasts (2026-05-18).**
  ✅ Earlier wear/remove broadcasts emitted one line per
  item — `wear all` on a 6-piece kit spammed bystanders with
  six lines. Refactored: `wear_into_silent` variant skips
  the per-item room line, `cmd_wear all` samples the
  equipped count before/after and emits a single "X dons N
  items of gear." Same shape for `cmd_remove all` ("X removes
  N items of gear."). Single-item wear/remove keeps the
  per-item line as before. Matches the pattern `cmd_drop all`
  established.
- **BANISH (extract effect) room broadcast (2026-05-18).** ✅
  The "extract" effect arm (used by BANISH and any future
  send-back-to-home-plane abilities) now snapshots the
  target name + room before despawning and broadcasts
  "X banishes Y back to the realm whence it came!" in bold
  magenta. Without it the creature blinked out silently —
  text-only clients had no narrative explanation.
- **RESURRECT room broadcast (2026-05-18).** ✅ The
  "resurrect" effect arm now broadcasts "X's spirit is yanked
  back from the void by Y." to room observers in bold cyan.
  The caster's "X's spirit returns to flesh." and the
  revived target's "Your spirit is yanked back into your
  body!" stay personal; the third-person room line is for
  everyone else. Pairs with the level-up / achievement
  broadcasts as a high-impact moment worth surfacing.
- **Achievement unlock room broadcast (2026-05-18).** ✅
  `grant_achievement` now emits "X earns the achievement 'Y'."
  to room observers in addition to the personal "Achievement
  unlocked" line. Skipped for hidden achievements (those are
  secrets the holder wouldn't want announced). Pairs with the
  level-up broadcast for celebratory moments.
- **Level-up room broadcast (2026-05-18).** ✅ When a player
  advances a level, the room now sees "X surges with newfound
  power and rises to level N!" (with the rank title appended
  when one is defined — staff tiers). The personal congratulation
  ("*** You have advanced to level N! ***" + practice point line)
  stays private to the leveler. Allies actually notice their
  party-mate ding up now rather than wondering why nothing
  happened after the kill.
- **Equipped gear on examine (2026-05-18).** ✅ `cmd_examine`
  now lists the target's worn items at the bottom of the
  output, sorted by `Slot::ORDER`. Reads "Equipped: a claymore
  (wielded), an iron helm (head), the leather boots (feet)."
  Lets players gauge a stranger's loadout before engaging
  (warhammer vs toothpick matters tactically). Mirrors
  `cmd_equipment`'s shape; skipped on items (their bound
  state is handled by `identify`). Live verified: AdminChar
  `examine testwarrior` → "Equipped: a claymore (wielded)."
- **Recite / wave / tap / quaff room broadcast (2026-05-18).**
  ✅ `invoke_object_abilities` (the shared path for scroll
  recite, wand wave, staff tap, and the bindings-quaff
  variant) now emits a third-person line ("X waves a willow
  wand.", "X recites from a parchment scroll.", "X taps a
  glowing staff.") to room observers before the spell
  effects fire. Bystanders see the gesture itself, not just
  the spell's downstream success line.
- **Crit tag in room hit broadcast (2026-05-18).** ✅ The
  third-person room broadcast on a swing was a generic "X
  hits Y." regardless of crit / normal; the attacker and
  target both saw "(critical hit!)" but bystanders didn't.
  Added a parallel `room_crit_tag` ("(critical!)" in bold
  red) appended to the room line on `SwingOutcome::Crit`.
  Damage stays hidden from mortal observers (info-leak
  policy) but the crit fact surfaces.
- **Follow / unfollow room broadcast (2026-05-18).** ✅
  `cmd_follow` and `cmd_unfollow` now emit a third-person line
  to room observers: "X falls into step behind Y." on follow,
  "X drops out of step from Y." on unfollow. Skipped on
  cross-room unfollow where the audience would have no
  shared frame. Same gap as guard had — silent state flips
  the rest of the party never saw.
- **Guard / unguard room broadcast (2026-05-18).** ✅
  `cmd_guard` now broadcasts "X moves to Y's side, ready to
  defend them." to the rest of the room when guarding starts,
  and "X steps back from Y's side." when guarding stops. The
  guarded target also picks up "X stops guarding you." on
  unguard (parallel to the existing "X stands ready to defend
  you." on guard-start). Allies see the formation forming and
  dissolving rather than silent state flips.
- **Room effect expiry broadcasts (2026-05-18).** ✅
  Magical-darkness, magical-light, and burning-room teardown
  branches in `effects.rs` now broadcast to every player in
  the affected room when the last backing instance expires:
  "The unnatural darkness dissipates." / "The magical
  radiance fades." / "The flames lash one last time, then
  sputter out." Without it, a hazard or environmental effect
  just silently lifted — a player waiting out CIRCLE_OF_FIRE
  had no signal it was safe to walk.
- **Login / logout text broadcast (2026-05-18).** ✅
  Disconnect path (`ConnRouter::on_disconnect`) and login
  path (the post-MOTD enter-world block) both already emit
  `Room.AddPlayer` / `Room.RemovePlayer` GMCP diffs to other
  clients in the room, but plain-telnet clients without GMCP
  got no signal. Added a paired text broadcast: "X fades
  into being, returning from dreams." on login and "X fades
  from view, retiring to dreams." on disconnect, going to
  every other player in the room.
- **`cancel wall` for room-applied wall effects (2026-05-18).**
  ✅ `cmd_cancel` previously only looked at effects whose
  `AppliedTo` was the player themselves, so room-applied wall
  EffectInstances were invisible to it — the caster (or anyone
  in the room) had no way to drop a wall short of waiting out
  the cast duration or splintering it via `doorbash`. Extended
  the filter to include effects whose `AppliedTo == player_room`
  AND whose name starts with `"wall-"`. Bash-style hazards
  (room burning, magical darkness) intentionally stay outside
  the cancel surface — those are environmental effects the
  caster expected to persist. Also patched the inline despawn
  path to clean the matching `RoomBlockedExits` entry before
  the entity drops; without it the wall stayed phantom in
  exits / look / movement until `effects_tick` no-op'd the
  missing instance. Live verified: cast wall, `cancel wall`,
  `exits` reads clean.
  - **Room broadcast on cancel.** The inline despawn path now
    also captures `(direction, kind_label)` and broadcasts
    "X gestures and the wall of stone north crumbles into
    nothing." to every other player in the room — same UX
    parity that natural expiry gets via the `effects_tick`
    teardown. Without it, other players in the room would
    notice the wall dropped only when they tried to walk that
    direction or ran `exits` / `look`.
- **WALL_OF_FOG / ILLUSORY_WALL.** ✅ Shipped 2026-05-18. Added
  `WallTraversal::{Block, Slow, Passable}` to `RoomBlockedExit`
  and routed all three variants through the same cast arm
  (`type=fog → Slow`, `type=illusion → Passable`, `type=stone|ice →
  Block`). `cmd_move`'s wall gate now branches:
  - **Block** — existing refusal path.
  - **Slow (fog)** — pays an extra `FOG_DRAG = 5` stamina toll,
    refuses if the player can't afford it ("the wall of fog
    drains you faster than you can push through"), emits "You
    push through the wall of fog, gasping in its choking mist."
  - **Passable (illusion)** — despawns the backing instance,
    drops the room entry, emits "You step through the wall of
    illusion — it ripples and dissolves into nothing." to the
    mover plus a "ripples and dissolves as X steps through"
    broadcast to the source room.
  Live verified: AdminChar cast ILLUSORY_WALL north → walked
  north (wall dissolved, arrived in Cobblestone Path); cast
  WALL_OF_FOG south → walked south (fog drag fired, arrived in
  Town Center).
- **HP-based bash.** ✅ Shipped 2026-05-18. `RoomBlockedExit`
  now carries an `hp: i32` field; the wall-cast site reads
  `override_params.hp` (default 100 ice / 200 stone) and stores it
  on the entry. `cmd_doorbash` checks the per-direction wall map
  *before* the door pipeline — if a wall blocks that direction,
  the swing deducts `5 + STR_bonus` from `wall.hp` instead. On
  `hp ≤ 0` the backing EffectInstance is despawned, the entry
  removed from `RoomBlockedExits`, and a "shatters into a
  thousand pieces" broadcast fires. Otherwise the player sees
  "You hammer the wall of ice north — it cracks but holds
  (~86 HP)." Live verified: AdminChar (STR bonus 9 → 14/hit)
  bashed ice wall through 100 → 86 → 72 → 58 across three swings.

## L — Summon family (2026-05-17)

User playtest ask 2026-05-17: "for summon, can you look at legacy. I
think you have to consent someone in order to be summoned. (Prevents
players from killing others by summoning them into high level zones.)"

**Legacy contract** (`fierymud_legacy/src/spells.cpp:2622-2715`):

The gate is a player preference, not a per-cast consent. Legacy uses
`PRF_SUMMONABLE` (default *off* → "summon protection on"). Toggled with
the `nosummon` command (semantically inverted: "nosummon" turns the
preference ON, allowing inbound summons). Bypasses: `PLR_KILLER` (PKers
have already opted into being targetable) and the server-wide
`summon_allowed` global (arena mode).

Gates that fire in order in legacy:
1. BFS pathfind, max distance = `skill / 5` rooms (fail → "too far away")
2. Same zone (different zone → same "too far away" fallthrough)
3. Target level ≤ `min(LVL_IMMORT, skill + 3)` — proficiency cap
4. Mob `MOB_NOSUMMON` or `MOB_NOCHARM` → "magic probing can't get a grip"
5. Destination room `ROOM_NOSUMMON` → "negating force blocks your spell"
6. Arena asymmetry (caster in arena, victim not) → refuse
7. PC `PRF_SUMMONABLE` gate (skipped if `summon_allowed` or target is PKer)
8. NPC saving throw vs spell → may fail

On success: dismount victim, "$n disappears suddenly." to old room,
char_to_room caster, "$n arrives suddenly." new room, "$n has summoned
you!" to victim, `WAIT_STATE 4 rounds` on caster, **summoned NPC turns
on caster with set_fighting**.

**Schema status (no new columns needed):**
- `PlayerFlag::NO_SUMMON` already exists (semantically inverted from
  legacy `PRF_SUMMONABLE` — `NO_SUMMON` set = summon-protected).
- `PlayerFlag::PK_ENABLED` is the bypass mirror of `PLR_KILLER`.
- `Room.allowsSummon` exists for the destination room gate.
- `MobBehaviors::NO_SUMMON` populated by the mob importer for legacy
  `MOB_NOSUMMON` (475 / 2139 mobs flagged in live DB 2026-05-17).
  `NO_CHARM` is NOT a `MobBehavior` value — the schema maps legacy
  `MOB_NOCHARM` to the per-mob `resistances.charm` JSON entry instead
  (311 mobs carry the resistance row). Both gates can read from
  their respective columns.
- `Permission::SUMMON` is the staff bypass.

**Implementation status (2026-05-17):**

- **L1. SUMMON gates + L1-v2 per-cast accept prompt.** ✅ Shipped
  2026-05-17 (revised same day per user playtest feedback). The
  player-target SUMMON spell rides on the teleport arm with
  `def.plain_name == "SUMMON"` guards. Gates 0-5: self-target,
  same-zone (proxy for legacy `skill/5` BFS — see L1.1), level cap
  (`target > caster + 3`), `MobBehavior::NoSummon`, destination
  `NoSummonRoom`, arena asymmetry. Gate 6 was originally a flat
  `NoSummon` refusal; replaced with three-way PC path:
  1. `NoSummon` set → silent auto-decline ("X is not accepting
     summons." to caster, nothing to target).
  2. `PkEnabled` (target opted into PvP) OR caster carries staff
     `Permission::Summon` → instant move (no prompt), with the
     existing post-move broadcasts + retaliation + 16s cooldown.
  3. Default PC → install `PendingSummon` marker carrying
     `from`, `from_name`, `dest_room`, `dest_room_name`,
     `at: Instant::now()`. Target sees "X is attempting to summon
     you to Y. Type ACCEPT to teleport, or DECLINE to refuse.
     (Auto-declines in 30 seconds.)" Caster sees "You send a
     summons to X. They have 30 seconds to accept."
  - `cmd_accept` / `cmd_decline` in `commands/info.rs` extended
    with summon-first checks before falling through to
    GroupInvite. On accept: teleports the target, fires
    depart/arrive broadcasts, applies the 16s caster cooldown,
    auto-looks the new room. On decline: removes marker, notifies
    both sides.
  - `pending_summon_tick` (in `commands::info`) scans `PendingSummon`
    every server tick, removes any over 30s old, notifies both
    parties of the expiry.
  - Three live-verified paths: ACCEPT (full move + auto-look),
    DECLINE (both sides notified), NO_SUMMON (caster refusal,
    target unprompted).
  - **Gate 7 (NPC save-vs-spell)** is deferred — save mechanics
    need a per-spell roll path that doesn't exist yet.
  - **L4 UX glitch** ("you complete the summoning ritual" fires
    before the gate refusal text) still applies to the gates 1-5
    failure paths but is harmless for the prompt path (the
    success line is followed by the "summons pending" notification,
    not a contradiction).
  - **L1.1 remote-name resolution.** ✅ Shipped 2026-05-17.
    `find_online_player_anywhere` (in `commands.rs`) is a global
    online-player lookup keyed on `Named.name` substring; it returns
    `Some(entity)` only on a unique match (ambiguous → None so the
    caster can disambiguate). The SUMMON-target-resolution branch
    falls back to it when `find_actor_in_room` misses, ahead of the
    non-violent default-to-self branch (otherwise every cross-room
    summon collapsed to self and L1.2's self-gate refused). Also
    duplicated into the pre-queue path: SUMMON with an unknown
    name refuses instantly with "There's no one named 'X' online
    to summon." rather than burning the 8s wind-up first. We don't
    do legacy's BFS distance cap (`skill / 5` rooms) — gates 1-5
    already enforce same-zone, which is a stricter spatial check
    than legacy used in practice.
  - **L1.2 pre-queue self-target check.** ✅ Shipped 2026-05-17.
    SUMMON with no target → "Summon who?" (actionable hint, not a
    refusal). SUMMON with explicit self-target ("me" / "self" /
    caster's display name) → "But you're already here!" Both fire
    pre-queue so a typo doesn't burn the 8s cast wind-up + slot
    cost. The dispatcher arm's Gate 0 stays as a safety net for
    the case where remote-name resolution finds a player whose
    partial-name happens to match the caster.
- **L2. `nosummon` command (revised semantics).** ✅ Shipped
  2026-05-17. The original L2 default-NO_SUMMON intent was
  reverted when L1 became prompt-based: new characters now spawn
  with empty `player_flags` (default = get the prompt). `nosummon`
  is now an opt-OUT — setting it makes incoming summons silently
  auto-decline without prompting (for players who don't want the
  interruption). Help text updated to match. fierylib seeder no
  longer pre-sets NO_SUMMON on test characters; live DB
  backfilled to remove it from the 6 chars that briefly had it.
- **L3 v1. SUMMON_xxx conjuration arm.** ✅ Shipped 2026-05-17.
  Added `"summon"` dispatcher arm: reads `mobType` from
  override_params and spawns the matching mob proto into the
  caster's room as a `Follower(caster)`. Hardcoded mobType→(zone,id)
  table: mount→(324,21), elemental→(52,12), demon→(510,24),
  greater_demon→(160,11), dracolich→(533,11), simulacrum→(163,8).
  Mob is bundled with the standard latent components (Sized,
  LifeForceTag, NaturalAttackType, MobTraits, MovementModeTag) to
  match the loader path. EffectInstance with `AppliedTo(mob)` and
  `name = "summoned-{mobType}"` drives the duration; on expiry
  effects_tick despawns the mob and emits a "fades back" line to
  room observers. `summon` added to `KNOWN_EFFECT_TYPE_ARMS` so
  K4's audit now reads clean. Live verified: AdminChar casts
  `summon demon` → "an Astral Demon" appears for 1125s.
  - **2026-05-17 follow-up:** ANIMATE_DEAD and CLONE both have
    empty `mobType` in override_params (just `type=creature`),
    which was hitting the "unknown mobType" refusal. Added an
    explicit ability-name fallback: ANIMATE_DEAD → (54, 20) "the
    Large Skeleton", CLONE → (163, 8) "the Knight Errant"
    (placeholder; real CLONE should mirror the caster's race/stats
    once target lookup lands). SIMULACRUM was already covered via
    its mobType="simulacrum" mapping. Live verified: AdminChar
    casts ANIMATE_DEAD → "the Large Skeleton" spawns as follower
    with 7500s duration.
    Real corpse-target ANIMATE_DEAD (consume a corpse from the
    room, scale the spawn to that creature's level) shipped 2026-05-17
    (see ANIMATE_DEAD follow-up below). Real actor-mirror CLONE
    shipped 2026-05-18 — the spawn block now overrides
    `mob_name` / `mob_keywords` / `mob_description` for CLONE
    casts, and the spawned HP mirrors the caster's max HP. Name
    reads "a clone of {caster}", keywords are `["clone", caster]`,
    description is "A flickering clone of {caster} stands here."
    The downstream broadcast and follower-summon diagnostic
    reuse the same `mob_name`, so the cast emits "You summon a
    clone of AdminChar to your side..." with no Knight Errant
    leak. Mob still inherits the proto's combat stats and
    natural damage — a real per-class scale (Warrior crit /
    Sorcerer spell-power) would need pulling more components
    from the caster, which can land later.

    **SIMULACRUM target-mirror** shipped 2026-05-18. Parallel
    branch to CLONE: resolves `target_word` via
    `find_online_player_anywhere` (the L1.1 helper), then
    overrides `mob_name` to "a simulacrum of {target}",
    keywords to `["simulacrum", target]`, description to "A
    wavering simulacrum of {target} stands here, eyes vacant."
    Falls back to mirroring the caster on miss/ambiguity so a
    typo still produces a recognizable spawn instead of the
    Knight Errant placeholder. Mob target lookup (cross-zone
    mob mirroring) is a follow-up — for now SIMULACRUM is
    player-only on the remote-mirror path. Live verified:
    AdminChar in zone 30 casts `simulacrum TestRogue` while
    TestRogue is in zone 100 → "A wavering simulacrum of
    TestRogue stands here, eyes vacant." spawns next to
    AdminChar.
  - **2026-05-17 follow-up:** ANIMATE_DEAD now consumes a target
    corpse when one is named: cast `animate dead <keyword>` finds
    a Corpse Item in the caster's room matching the keyword and
    despawns it before raising the skeleton. Bare cast (no arg)
    still spawns the default skeleton.
    **Player-corpse safeguard:** new `PlayerCorpse` companion
    marker (added at player death in `combat.rs::handle_death`)
    lets ANIMATE_DEAD distinguish player corpses from mob
    corpses. When the only matching corpse is a player corpse, the
    cast refuses with "Necromancy on a fellow adventurer's
    remains? Some lines aren't crossed." Live verified.
    **Player-corpse looting protection:** parallel gate in
    `cmd_get` refuses non-staff looters from another player's
    corpse with "X isn't yours to loot — disturbing another
    adventurer's remains requires their consent." Staff bypass via
    `is_staff`. PlayerCorpse marker is round-tripped through the
    corpse snapshot (save+load includes `is_player: bool`) so the
    consent gate survives restarts.
    **HP scaling:** new `CorpseOriginLevel(i32)` component carries
    the dead actor's level. Set at both mob and player corpse
    spawn. ANIMATE_DEAD reads it and scales the spawned skeleton's
    HP by `(level / 5).clamp(1, 5)` — animating an L10 mob gives
    a 2× skeleton, L25+ caps at 5×. Round-tripped through the
    corpse snapshot too. Live verified L1 = 1× (10 HP base).
- **L4. UX glitch — pre-loop success template fires before gate
  refusal.** ✅ Shipped 2026-05-17. Restructured the success-line
  emission: damage spells (any AbilityEffect with effect_type=
  "damage") still fire pre-loop so death broadcasts come after the
  cast confirmation; non-damage spells defer the header into
  `pending_header` and emit it post-loop ONLY when at least one
  applied_msg lacks a refusal signature. Refusal substrings
  recognized: "(refused", "not accepting", "undead", "no life",
  "self target", "self-drain", "(no ", "(unknown", "(absorbed",
  "(already there", "(no direction", "(target busy/NoSummon",
  "(different zone", "(level cap", "(mob NoSummon", "(room
  NoSummon", "(arena asymmetry", "(target not dead". Live verified:
  SUMMON on NoSummon-protected target shows just "TestRogue is
  not accepting summons." with no leading "You complete the
  summoning ritual."; PROT_FROM_EVIL success still shows
  "You protect TestRogue from evil."
- **L5. Cast success template confusion.** Originally the SUMMON
  spell's `success_to_caster` was "You vanish in a flash of light!"
  — copy-pasted from generic teleport but wrong (the caster
  doesn't move; the target does). Updated 2026-05-17 to "You
  complete the summoning ritual." and `success_to_room` to
  "{actor.name} completes a summoning ritual." Same fix should
  be reviewed for SUMMON_xxx variants once L3 lands.

## Sequencing recommendation

Order if you want to maximize player-visible parity in the shortest path:

1. **A (combat balance)** — A1/A2/A4/A5/A6/A7 all shipped. Watch for live-play tuning surprises.
2. **B (parity wires)** — remaining B2 (bashable doors, needs `ExitData.hit_points` plumb), B3 (aggressionFormula). B1/B5/B6 shipped.
3. **H follow-ups** — quick wins from the last playtest.
4. **C (flavor text)** — easy wins; one loader + one renderer per item.
5. **D items** ship as their surrounding systems ship (don't force).
6. **E** — improvements; pick whichever the player base would notice.
7. **F decisions** — answer when convenient; none are blocking.

## Rest / Repose system (2026-05-17)

New feature. Design doc:
[`rest-and-repose.md`](rest-and-repose.md). ADR for the four
surprising-against-MMO-default decisions:
[`../adr/0001-rest-system-tradeoffs.md`](../adr/0001-rest-system-tradeoffs.md).
Vocabulary in
[`/muditor/CONTEXT.md`](../../../muditor/CONTEXT.md) under "Resting
and Repose": `Repose`, `RestSource`, `Refreshed Effect`, `Wake
Effect attachment`.

**Sequencing:** RR1-RR2 (schema + migration) is owned by fierylib's
remaining-work doc and ships first; everything below depends on it.

- **R1. sqlx struct updates.** Update `mud-db` row structs for
  `Characters` (`repose`, `restSource`, `restTier`), `Rooms`
  (`isInn`, `innName`, `innTiers`), `Objects` (`campKitTier`).
  Regenerate `sqlx-data.json` after schema is live.
- **R2. RestSource acquire commands.**
  - `cmd_rent` in `mud-server/src/commands/`: gates on
    current room's `isInn`; with no arg, prints the tier menu
    from `innTiers`; with `<tier-name>` arg, validates affordability,
    deducts gold, sets `Characters.restSource=INN`,
    `Characters.restTier=<chosen>`. Confirm prompt for tier > 1.
  - Extend `cmd_camp` in `mud-server/src/camp.rs` to accept an
    optional kit Object name (`camp <kit-name>`). Kit lookup at
    setup start, **consumed only at camp completion** (not on
    interrupt). On completion, set `restSource=CAMP` and
    `restTier=computeCampTier(class, group, kit)`. See design
    doc §"Camp tier computation" for the formula.
  - Disconnect / `quit` path: if `restSource == NONE`, stamp
    `restSource=QUIT, restTier=0`. Do not overwrite if a source
    is already set (player rented before quitting).
  - Logout from a HOUSE-owned room: defer until housing schema
    exists (see design doc "Open / deferred"). For v1, leave as
    a TODO comment in the disconnect path.
- **R3. Login fill + relocation flow.** In
  `mud-server/src/login.rs`:
  - Compute Repose accrual: `elapsed_hours * ratePerHour(restTier)`,
    clamped by `capPercent(restTier) * xpForNextLevel(level)`.
    Add to `Characters.repose`.
  - Spawn-room decision: if `restSource in {CAMP, INN, HOUSE}`,
    spawn at `currentRoom`; else if elapsed >= 30 min, spawn at
    `recallRoom`; else `currentRoom`.
  - **Do not consume `restSource` yet** — wait for first XP gain.
- **R4. First-XP-gain wake consumer.** Hook the XP-gain path
  (combat.rs reward arm + quest reward arm + any other gain
  sites). When a character with `restSource != NONE` gains XP:
  1. Spawn `Refreshed` Effect (skip if `QUIT`). Strength = current
     `restTier`. Duration TUNABLE (default 1800s).
  2. Apply Wake Effect attachments keyed on source kind:
     `INN` → `RoomWakeEffects` on the room where logged off,
     filtered by `minTier <= restTier`.
     `CAMP` → wake-effect rows captured from the consumed kit
     at camp completion (stash on a transient component, or
     re-read from the just-consumed `ObjectWakeEffects`).
     `HOUSE` → `ObjectWakeEffects` from the bed Object in the room.
  3. Clear `restSource=NONE, restTier=0`.
- **R5. Repose XP math.** Implement in the XP-gain pathway:
  `bonus = base_xp * (REPOSE_MULTIPLIER - 1); drawn = min(bonus,
  character.repose); character.repose -= drawn; return base_xp +
  drawn`. `REPOSE_MULTIPLIER` is TUNABLE (default 2.0). Log
  lifetime repose-XP spent on the character for stats.
- **R6. Refreshed Effect runtime hooks.** Bind Lua hooks in the
  scripting layer to read attachment `strength` (1-3), capture
  base regen rates on apply, add proportional `RegenBonus`
  per tick, remove on expiry. Use existing `RegenBonus`
  component (`mud-world/src/components.rs`). See design doc
  §"Refreshed Effect" for the per-tick formula.
- **R7. Wake Effect attachment loader.** New module
  `mud-world/src/wake_effects.rs`. Two query functions:
  `room_wake_effects(zone, id, restTier) -> Vec<WakeRow>` and
  `object_wake_effects(zone, id) -> Vec<WakeRow>`. Called by R4.
- **R8. Follow-up (do NOT block v1): hard-coded class check →
  data.** Replace `matches!(class, Class::Ranger | Class::Druid)`
  in camp tier computation with a `CharacterClass.campcraftBonus`
  column read. Migrate when ranger/druid class rows land in DB.

**Blocker:** fierylib RR1-RR2 (schema + migration). After that,
R1-R7 can ship together. R8 is a follow-up.

## How to use this doc

When something here ships, **delete** the bullet. This doc should always reflect "what's still pending." If something gets dropped (decided not worth doing), move it to a "Cancelled" section so the history survives.

Historical context for completed work: `migration-plan.md`, `parking-lot.md` (Resolved section), `database-audit.md`.
