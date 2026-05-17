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

- **Create Food per-class selection.** Legacy `spell_creations`
  picks a base proto by caster class — Cleric/default → zone 120,
  Paladin → 110, Priest → 100, Anti-Paladin → 130, Druid → 140 —
  then adds 0..9 from skill scaling. Today the runtime `create`
  arm pulls a single hard-coded proto (currently the waybread
  fallback, zone 185 id 8). Plumb caster.class_id + skill into
  the create arm so the right per-class roster fires. Zones
  100/120 are sparsely imported (3-4 protos each) so the content
  side also needs work.
- **Minor Creation arg lookup.** Legacy `spell_minor_creation`
  reads the cast arg ("dagger", "robe", "spellbook", ...) against
  a 40-entry `minor_creation_items[]` table (constants.cpp:28)
  and spawns `(zone 10, id i)` for the matching index. All 40
  protos are imported under zone 10. The runtime would need
  the cast arg threaded through to the create arm and a copy
  of the keyword table. Today MINOR_CREATION just spawns the
  hard-coded mushroom default regardless of arg.
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
- **I.3 Seeded test users have no starting gear.** Partially
  resolved via live-DB hand-patch: TestWarrior now wields a
  claymore (zone 163, id 0), TestRogue a small silver dagger
  (zone 557, id 63). Both are tier-appropriate. TestMage stays
  unarmed (sorcerer kit is a follow-up). Verified live:
  TestWarrior L25 with claymore lands 57 dmg per hit at ~33% hit
  rate vs L17 frost stallion — combat math reads off the weapon
  (wpn=21 ×AP+125%=47 ±var=57). The fierylib seeder
  (`src/fierylib/seeders/user_seeder.py`) should still grant
  this at create time so a `seed users` from scratch matches.
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

- **K2. `HALF_DAMAGE` on-save action arm.** Today the runtime's
  save dispatcher (`commands.rs::on_save_action_for(...)`) only
  understands `NEGATE` (skip effects entirely) and `HALF_DURATION`
  (halve EffectInstance duration). Unknown actions fall through to
  `Failed` (effects apply in full). Damage spells in legacy
  fierymud halved damage on save (`if (mag_savingthrow(...)) dam >>= 1;`
  in `magic.cpp`). Content authored all damage-spell saves as
  `NEGATE` for now — engine ask is to add a `HALF_DAMAGE` arm that
  applies `dam / 2` when present. Once the arm lands, the content
  side can sweep `data/abilities.json` and re-balance damage saves
  from `NEGATE` to `HALF_DAMAGE` (one-line change per spell).

  - Scope: extend the SaveOutcome enum + the per-action match arm
    that wraps the damage-apply call; thread the half flag into
    `apply_damage` so resistance/penetration math still run on the
    halved value.

- **K3. Authored saves only — runtime save dispatcher must roll.**
  Verify the save roll itself runs at cast time before applying
  effects/damage. If `AbilitySavingThrow` rows aren't consulted in
  `invoke_ability` today, all 126 newly authored saves are inert.
  Quick smoke: cast a tagged damage spell at a high-WIS target and
  watch for "you save" messaging.

## J — Resistance / protection extensions (2026-05-17)

Content-side: `ObjectResistance` is now populated for the legacy
single-element protection effects (PROT_FIRE/COLD/AIR/EARTH +
FIRESHIELD/COLDSHIELD + NEGATE_*). Two legacy protection shapes can't
be expressed in the current `ObjectResistance` schema:

- **J1. Spell-circle absorb (MINOR_GLOBE / MAJOR_GLOBE).** Legacy:
  minor globe absorbs spells of circle ≤ 3, major globe ≤ 6 (rough
  values — check `magic.cpp` for exact thresholds). Equipped items
  carrying `EFF_MINOR_GLOBE` / `EFF_MAJOR_GLOBE` therefore don't grant
  a per-element resistance — they short-circuit hostile spell
  application up to a tier threshold. Options:
  1. Add `Character.spell_absorb_max_circle: int` column populated as
     `max(spell_absorb_max_circle across equipped items)`, gate
     incoming spell damage in `invoke_ability_with` before the resist
     pipeline runs.
  2. Push it into `ObjectResistance` as a synthetic element
     (`ARCANE_ABSORB_MINOR` / `_MAJOR`?) — messier.
  Option 1 is cleaner. ~10 items in legacy carry these.

- **J2. Alignment-vs-alignment protection (PROTECT_EVIL /
  PROTECT_GOOD).** Roughly 45 legacy items wear `PROTECT_EVIL` or
  `PROTECT_GOOD`. Legacy mechanic: 25% damage reduction from incoming
  attacks by aligned (evil / good) attackers, *regardless* of damage
  element. This is alignment-keyed, not element-keyed, so it can't
  ride on `ObjectResistance.element`. Cleanest: an
  `Object.protect_alignment` column (or a parallel
  `ObjectAlignmentResistance` table) consumed in the combat pipeline
  after the per-element step, multiplying by 0.75 when the attacker's
  alignment matches. Content side will follow whichever schema lands.

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
- **K2. Sentence-start capitalization sweep.** Every line that starts
  with "the X / a X / an X dies/leaves/arrives/etc" should run through
  `cap_sentence_start`. Death/leave/arrive messages already do
  partially; combat hit-broadcasts and trigger emits don't. Grep
  `crates/mud-server` for `format!\("(the |a |an )` outside of
  template renders and route them through.
- **K3. Info-leak audit (mortal vs god view).** Today consider/score
  /look/inspect can leak exact HP/level/alignment for mobs to mortal
  players. Gods (Builder+ role) should still see numbers; mortals get
  the "impression" form. Plumb a single `viewer_is_staff(world, p)`
  helper through the renderer for those views, and gate raw integers
  behind it.
- **K4. Dead-spell audit.** Walk AbilityCatalog at boot, flag every
  SPELL whose AbilityEffect list is empty or whose effect_type isn't
  in the dispatcher's match arms (damage / heal / cleanse / stun /
  dispel / redirect / stop_combat / create / portal / modify /
  intercept / extract / dismount / teleport / reveal / knockdown).
  Today those cast successfully and emit the success line but produce
  no in-game effect — a content gap that should be visible. Log
  warn-level on boot with the list. The content fix lives in fierylib
  (data side); see `fierylib/remaining_work.md` §K4 follow-up.

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

- **L1. SUMMON dispatcher gates.** ✅ Shipped 2026-05-17. The
  player-target SUMMON spell rides on `effectType=teleport, scope=
  target, destination=caster` — same teleport machinery, but with
  a guarded gate set in `commands.rs` `"teleport"` arm that fires
  only when `def.plain_name == "SUMMON"`. Gates 0-6 wired:
  self-target refusal, same-zone (proxy for legacy `skill/5` BFS),
  level cap (`target > caster + 3`), `MobBehavior::NoSummon`,
  destination `NoSummonRoom`, arena asymmetry, and PC `NoSummon`
  preference (bypassed by `PkEnabled` or staff `Permission::Summon`).
  Post-move: depart/arrive `broadcast_room_visual`, "X has summoned
  you!" private line to victim, NPC retaliation via
  `engage_combat(victim, caster, dest_room)`, and a 16s (4 combat
  rounds) cooldown insert on the caster so the spell can't be
  spammed. Live-verified: AdminChar casts summon on TestRogue
  (default NoSummon) → caster sees refusal, TestRogue sees
  hint "Type NOSUMMON to allow other players to summon you." and
  doesn't move. **Gate 7 (NPC save-vs-spell)** is deferred — save
  mechanics need a per-spell roll path that doesn't exist yet.
  **Follow-up L1.1: remote-name resolution.** Today targeting
  defaults to in-room name lookup; if the target isn't in the
  caster's room it falls through to self. Legacy uses
  `find_track_victim` (BFS, max distance = `skill/5`). Without
  that, SUMMON can't reach someone in another room — defeats the
  spell's purpose. Add a remote-actor name lookup helper and
  thread it through targeting for `SUMMON`-class spells.
- **L2. `nosummon` command + default-protected.** ✅ Shipped
  2026-05-17. `nosummon` command already existed
  (`commands/info.rs::cmd_nosummon`); refreshed the help text to
  describe the gate and the PK bypass. `mud_db::characters::create`
  now inserts `player_flags = ARRAY['NO_SUMMON']` so new characters
  are summon-protected by default. fierylib `user_seeder.py` adds
  the same default to test characters. Live DB backfilled with
  one UPDATE — all 7 existing characters (AdminChar, BuilderChar,
  Strider, TestWarrior/Cleric/Mage/Rogue) flipped to NO_SUMMON.
- **L3. SUMMON_xxx flavor family** (7 spells: SUMMON_DEMON,
  SUMMON_ELEMENTAL, SUMMON_DRACOLICH, SUMMON_GREATER_DEMON,
  SUMMON_MOUNT, SUMMON_CORPSE, SPHERE_SUMMON). These use
  `effectType=summon` and hit the dispatcher catchall today (mob
  proto spawn not implemented). Plumb a per-class lookup table
  (similar to CreationRecipe in fierylib doc §8):
  `SummonRecipe(abilityId, classId, mobZoneId, mobId, minSkill)`.
  Runtime spawns the mob into the caster's room as a charm
  follower. Defer.
- **L4. UX glitch — pre-loop success template fires before gate
  refusal.** The cast confirmation ("You complete the summoning
  ritual.") emits unconditionally before the dispatcher arm runs.
  When gates refuse, the caster sees both lines — confusing.
  Fix would require either a per-spell suppression flag or a
  restructure to defer the success emit until the arm decides.
  Documented; not blocking.
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
