# Gear curves & combat balance audit

**Status:** in progress (Iteration 1)
**Owner:** combat tuning team
**Scope:** what does "average gear" actually look like at each level tier, how does that compare to mob power, and is the new combat system delivering the experience design intends ("solo to L20, grouping from there")?

## Why this doc exists

A solo-combat sweep (TestWarrior vs same-level trash) was showing breakdowns at L15 with my stat-injection sim of "average gear". Two questions came out of that:

1. Was my gear simulation way off from what a real player at level N would actually be holding?
2. How does today's curve compare to the legacy CircleMUD combat math we migrated from — are we still in the same neighborhood, or did we drift?

This doc captures the data so we can make tuning calls from facts, not feel.

---

## 1. Weapon drop curve (modern DB, current state)

Source: every `Object.type='WEAPON'` reachable via a `MobReset → MobResetEquipment` chain (i.e. things mobs are *seeded to wear*, not just random untracked items). Damage figure = expected per-swing value of the `Hit Dice` jsonb (`num × (size+1) / 2`). `HitRoll` is the per-weapon authored hit-roll bonus that converts to accuracy via the new `accuracy = 50 + hit_roll*2 + level*2` formula.

| Tier | Mob lvls | n | Med dmg | P75 | P90 | Max | Avg HitRoll |
|---|---|---|---|---|---|---|---|
| **T1** | 1-10 | 169 | 3.5 | 4.0 | 5.0 | 12.0 | 0.04 |
| **T2** | 11-20 | 213 | 5.0 | 6.0 | 6.0 | 24.0 | 0.69 |
| **T3** | 21-30 | 182 | 7.5 | 7.5 | 7.5 | 22.0 | 0.33 |
| **T4** | 31-40 | 121 | 8.0 | 9.0 | 25.0 | 45.0 | 0.05 |
| **T5** | 41-50 | 89 | 9.0 | 10.0 | 12.0 | 50.0 | 0.03 |
| **T6** | 51-70 | 88 | 8.5 | 12.0 | 40.0 | 60.0 | 0.02 |
| **T7** | 71-99 | 145 | 5.0 | 7.5 | 14.6 | 97.5 | 0.00 |

### Observations

- **Median damage doubles roughly every 20 levels** (T1=3.5 → T3=7.5 → T5=9.0), but mob HP **multiplies by ~7×** across the same span (L10 113 HP → L30 1058 HP → L50 2666 HP). Damage isn't keeping pace.
- **T7 (L71-99) median damage is *lower* than T3-T6.** That isn't a bug — high-level mobs often drop low-level junk or non-weapon "WEAPON"-typed items. The signal that matters at endgame is P90 (14.6) and max (97.5), which show the BIS curve is there if a player hunts for it. But the *typical* drop a casual high-level player picks up is unimpressive.
- **HitRoll is effectively zero across every tier.** Average weapon HitRoll never crosses 1.0. So the `+ hit_roll * 2` term in `accuracy = 50 + hit_roll*2 + level*2` is ~0 for almost every player weapon, which means a player's *only* accuracy bump per level comes from the `+level*2` baseline. Mobs get the same baseline *plus* their authored hit_roll (often 20+ on trash mobs and bosses), so mobs structurally out-scale players on hit rate.

---

## 2. Drop volume by tier × type

Confirms armor exists at every tier; absolute counts shouldn't be confused with per-mob loot quality.

| Tier | Armor | Weapon | Treasure |
|---|---|---|---|
| T1 | 614 | 169 | 73 |
| T2 | 195 | 213 | 29 |
| T3 | 291 | 182 | 27 |
| T4 | 117 | 121 | 26 |
| T5 | 130 | 89 | 29 |
| T6 | 59 | 88 | 27 |
| T7 | 229 | 145 | 40 |

**T6 (L51-70) has the thinnest armor pool** (59 items) — worth flagging, may need authoring attention.

---

## 3. Armor curve (modern DB, current state)

Source: every `Object.type='ARMOR'` reachable via a mob-drop chain. Legacy `AC` value is stored in `Objects.values->>'AC'` as a positive integer (higher = better armor in *this* schema; the sign was flipped during import — note this differs from raw CircleMUD AC, where lower was better).

| Tier | Mob lvls | n | Med AC | P75 | P90 | Best |
|---|---|---|---|---|---|---|
| **T1** | 1-10 | 614 | 1 | 2 | 4 | 10 |
| **T2** | 11-20 | 195 | 4 | 5.5 | 7 | 8 |
| **T3** | 21-30 | 291 | 5 | 10 | 11 | 15 |
| **T4** | 31-40 | 117 | 7 | 8 | 8 | 18 |
| **T5** | 41-50 | 130 | 6 | 8 | 12 | 18 |
| **T6** | 51-70 | 59 | 7 | 19 | 20 | 24 |
| **T7** | 71-99 | 229 | 4 | 8 | 12 | 45 |

If we equip ~6 armor pieces at the tier median, the totals look like:
| Tier | Total stacked AC | Expected armor_pct (5×AC, clamped 100) |
|---|---|---|
| T1 | 6 | 30% |
| T2 | 24 | 100% (capped) |
| T3 | 30 | 100% (capped) |
| T4 | 42 | 100% (capped) |
| T5 | 36 | 100% (capped) |
| T6 | 42 | 100% (capped) |
| T7 | 24 | 100% (capped) |

Looks great on paper — *but see §3a below.*

### 3a. 🚨 The `ObjectEffects` table is empty in the current DB

The whole table — `SELECT COUNT(*) FROM "ObjectEffects" WHERE effect_id IN (SELECT id FROM "Effect" WHERE "effectType"='modify')` → **0 rows**. In fact, `SELECT COUNT(*) FROM "ObjectEffects"` → 0 total.

This is a critical migration bug: **armor's `values.AC` is sitting in the DB but the equip path never reads it.** `equip_apply.rs::apply_object_to_wearer` iterates `ObjectEffects` rows; if none exist for an item, no stats get applied. So when a player wears full plate, they get *nothing* added to their `armor_pct`/`armor_flat`/`hardness`/`accuracy`/etc. — only the weapon's dice apply (because dice are read directly from `Objects.values`, bypassing the effects pipeline).

Evidence trail:
- Earlier in this same session (before the `full_reset_and_import.sh` rerun), the server boot log emitted ~100s of warnings like `ObjectEffect modify: unsupported target, skipped target=str_bonus`. That means rows *did* exist at that point; they were just hitting an old code path that didn't recognize the targets.
- After fixing `apply_modify_delta` (commands.rs) to support those targets, the warnings went away — but at the *same time*, the re-import wiped the rows. So the warning disappearance masked the loss.

**Root cause (confirmed)**: `fierylib/src/fierylib/importers/object_importer.py::import_affect` writes to `self.prisma.objectaffects` — the **`ObjectAffects` table no longer exists** in the schema (dropped during a Wave migration; replaced by `ObjectEffects` + a junction-table-with-modify-Effect pattern). Every call fails silently inside the try/except, returns `{"success": False}`, the caller increments a "failed" counter but the import continues. So all 604+ apply blocks per zone are dropped on the floor.

Evidence:
- `SELECT 1 FROM information_schema.tables WHERE table_name = 'ObjectAffects'` → empty.
- `Effect` table has a row `(id=3, effectType='modify', name='modify')` waiting to be referenced.
- Legacy `.obj` files in `~/Code/mud/lib/world/obj/*.obj` carry `A\n<applyType> <modifier>` blocks (CircleMUD's standard apply format).
- The `_LEGACY_AFFECT_MAP` dict in `object_importer.py:408` only covers `AC`/`HITROLL`/`DAMROLL`, not the full `ApplyTypes` enum from `mud/types/__init__.py:542` (Str/Dex/Int/Wis/Con/Cha/Hit/Mana/Move/Saving*/Focus/etc.).
- Even for the three covered keys, the lookup uses `affect.get("location", "")` which is keyed differently from the parser's output (`str(ApplyTypes(apply))` produces strings like `"ApplyTypes.HitRoll"`, not `"HITROLL"`) — so the map *never matches* anyway.

### Fix shape (to be drafted in next iteration after parser/affect data flow is traced end-to-end)

1. Replace `import_affect` body with a write to `ObjectEffects`:
   ```python
   await self.prisma.objecteffects.create(data={
       "objectZoneId": obj_zone_id,
       "objectId": obj_vnum,
       "effectId": MODIFY_EFFECT_ID,   # cache the (id=3) row at startup
       "strength": 1,
       "modifierData": {"target": target, "amount": modifier},
   })
   ```
2. Replace `_LEGACY_AFFECT_MAP` with a full `ApplyTypes → target_name` table matching `apply_modify_delta` in `mud-server/src/commands.rs` (legacy-alias targets `"ac"`, `"hitroll"`, `"damroll"` already auto-scale on the Rust side; bonus stats use `"str_bonus"`, `"dex_bonus"`, …; pools use `"max_hp"`, `"max_mana"`, `"max_stamina"`).
3. Verify the parser key (`"location"` value) matches: `read_applies` returns `applies[str(ApplyTypes(apply))] = val` → keys are like `"ApplyTypes.HitRoll"`. Decide whether to normalize in the parser or in the importer.

Until this is fixed, every "average gear" combat measurement is measuring a *worse* player than a real one with full gear would be — because real gear contributes nothing here either.

### 3b. ✅ Fix landed (this session): `import_affect` now writes `ObjectEffects`

Edit: `fierylib/src/fierylib/importers/object_importer.py:407-…` (rewrites `_LEGACY_AFFECT_MAP` to the full AFFECTS table and `import_affect` to write `ObjectEffects(modify, modifier_data={target, amount})`).

| Legacy AFFECT | Modern target | Notes |
|---|---|---|
| `STR` … `CHA` | `str_bonus` … `cha_bonus` | direct |
| `MANA` / `HIT` / `MOVE` | `max_mana` / `max_hp` / `max_stamina` | pool maxes |
| `AC` | `ac` | sign flipped (legacy lower=better → modern positive) |
| `HITROLL` / `DAMROLL` | `hitroll` / `damroll` | Rust auto-scales these legacy aliases (×2 for accuracy, ×5 for attack_power) |
| `SAVING_*` | `saving_*` | direct |
| `HIT_REGEN` / `FOCUS` / `PERCEPTION` / `HIDDENNESS` | matching modern target | direct |
| `NONE`, `CLASS`, `LEVEL`, `AGE`, `CHAR_WEIGHT/HEIGHT`, `GOLD`, `EXP`, `SIZE`, `COMPOSITION` | — (skipped) | no combat impact / reserved |

Modify-effect row id is looked up lazily on first call and cached on the importer instance. Zero-modifier blocks are skipped to avoid useless no-op rows.

**Verified on zone 4** (a high-affect zone, 604 `A` blocks in the legacy `.obj` file): re-import with `poetry run fierylib import-legacy --zone 4 --clear` produced **602 `ObjectEffects` rows** (2 dropped — zero-modifier and `NONE`-type entries skipped on purpose). Sample target distribution from zone 4:

| Target | rows | avg modifier |
|---|---|---|
| `focus` | 98 | +3.3 |
| `damroll` (→ attack_power × 5) | 68 | +2.6 |
| `hitroll` (→ accuracy × 2) | 63 | +2.0 |
| `dex_bonus` | 46 | +3.0 |
| `max_hp` | 43 | +44 |
| `int_bonus` | 41 | +3.0 |
| `cha_bonus` | 36 | +2.4 |
| `str_bonus` | 30 | +3.4 |
| `wis_bonus` | 28 | +3.7 |
| `max_stamina` | 27 | +35 |
| `hit_regen` | 26 | +17 |
| `con_bonus` | 24 | +2.0 |
| `perception` | 23 | +82 |
| `saving_spell` | 18 | -9.7 (cursed items) |
| `saving_breath` | 13 | -10.7 (cursed items) |

Implication for the gear-curve table in §1: those numbers will look completely different once the full re-import lands and the same query runs through `ObjectEffects` aware. A typical T2-T3 character with realistic gear is going to have +20 hitroll (= +40 accuracy via the ×2 scaler in `apply_modify_delta`), +15-20 damroll (= +75-100 attack_power via the ×5 scaler), +100-200 max_hp from items, and 3-4 points of every core stat. That's a much bigger stick than my level\*3 stat injection was using.

Full re-import in progress (this session) — §1, §3, and §5 will be re-derived from the post-fix data set.

### 3c. ✅ End-to-end equip verified

After the full reset+reimport with the patched importer:
- DB has **4044 `ObjectEffects` rows** spanning targets `focus`, `max_hp`, `hitroll`, `damroll`, all six `*_bonus` stats, `max_stamina`, `saving_*`, `hit_regen`, `perception`, etc.
- Server starts clean: **0 `unsupported target` warnings** — every target the importer now writes is supported by `apply_modify_delta`.
- Smoke test: TestWarrior (L25, 250 HP) wore the L12 "leather jerkin of the 3rd Black Legion" (`(zone 55, id 27)`, +5 max_hp + +2 con_bonus per apply blocks). HP went 250 → **255** immediately. Wiring is alive end-to-end.

### 3d. 🚨 Still broken: `Objects.values.AC` (base armor type AC) is never read

Grep confirms it — `Objects.values->>"AC"` is *only* referenced inside test assertions in `equip_apply.rs:465` and a migration comment in `commands.rs:11950`. No production read path. So:

- A leather jerkin imported as `Objects.values = {"AC": 4}` has AC 4 sitting in the row.
- Only the *apply-block* effects (the things now landing in `ObjectEffects`) feed into the wearer's stats.
- For an item whose entire armor value lives in `values.AC` (no apply blocks), wearing it gives the player nothing armor-side.

The runtime needs an analogue of weapon-dice loading: when wearing an item, read `Objects.values.AC` (legacy semantic) and contribute it to `CombatStats.armor_pct` (probably via the existing `ac → armor_pct ×5` legacy alias in `apply_modify_delta`). Recommended pattern: spawn a synthetic `ObjectEffects(modify, target=ac, amount=values.AC)` at item-load time inside `equip_apply::apply_object_to_wearer`, or just inline a single `apply_modify_delta(world, wearer, "ac", values.AC)` call before iterating the rest of the effects rows. Either is a small contained change.

## 4. Realistic "fully-geared player" profile per tier (post-fix)

Computed: for each tier, the **expected total stat bonus a player gets by wearing ~6 random-tier-drop items**. Calculated from `(rows_per_item × avg_per_row × 6_slots)` for each target. Targets shown are the ones that flow into combat math today (post §3a fix, before §3d fix).

| Tier | acc (+) | attack_pwr (+) | max_hp (+) | evasion (+) | str (+) | con (+) | armor_pct |
|---|---|---|---|---|---|---|---|
| T1 (L1-10) | 1.6 (from 0.8 hitroll) | 3.0 (from 0.6 damroll) | 16 | 8 (from 1.6 dex) | 1.0 | 2.8 | ~0 |
| T2 (L11-20) | **4** (2.2 hitroll) | **7** (1.4 damroll) | 12 | **13** (2.6 dex) | 2.4 | 3.0 | ~0 |
| T3 (L21-30) | 1.0 (0.5 hitroll) | 5.5 (1.1 damroll) | 17 | 4.5 (0.9 dex) | 0.8 | 19 | ~0 |
| T4 (L31-40) | 1.6 (0.8 hitroll) | 4.4 (0.9 damroll) | 13 | 2.6 (0.5 dex) | 5.1 | 3.5 | ~0 |
| T5 (L41-50) | 5.1 (2.5 hitroll) | 8.6 (1.7 damroll) | 14 | -1.6 (curse?) | 2.0 | 3.2 | ~0 |
| T6 (L51-70) | -0.3 (-0.2 hitroll) | 11.4 (2.3 damroll) | 10 | 0.9 (0.2 dex) | 1.1 | 2.0 | ~0 |
| T7 (L71-99) | 3.5 (1.7 hitroll) | 13 (2.6 damroll) | 44 | 2.4 (0.5 dex) | 0.6 | 2.4 | ~0 |

### Reading this table

- The L20 (T2) median-geared character gets **+4 accuracy / +7 attack_power / +12 max_hp / +13 evasion** from a typical equip set. **My earlier sweep used `attack_power = level*3 = 60`** — that was *significantly over-modeling* gear, not under-modeling. So with realistic gear the warrior is *weaker* than the L15-stallion-loss sweep suggested.
- `armor_pct ≈ 0` across every tier: the apply-block AC rows tend negative (after sign-flip, fewer than 10 items per tier have a positive AC modify), and `values.AC` isn't being read at all (§3d). So players take **full damage** from mobs at every level today.
- The `con` jump at T3 comes from the dragon-zone equipment sets (legion belts, jerkins). It's a per-zone authoring quirk, not a curve-design intent.

## 5. Combat math vs mob curves (post-fix)

Using the T2 profile for an L20 warrior, against an L20 trash mob (avg 437 HP, avg 21.5 dmg, accuracy 90 from new formula, evasion 90 from new formula):

- Warrior accuracy = `50 + 0 hit_roll + 20×2 + 4 gear = 94`; mob evasion = 90 → hit rate = `50 + (94-90)/2 = 52%`
- Warrior swing damage = `1d7 × (1 + 7/100) ≈ 4.3` → `0.52 × 4.3 = 2.2 dmg/round`
- Time to kill L20 mob: `437 / 2.2 = 199 rounds` (≈ 3.3 minutes per fight)
- Mob accuracy = `50 + 20×2 + 20×2 = 130`; warrior evasion = `50 + dex_bonus×5 + 40 + 13 gear = ~108` → hit rate = `50 + (130-108)/2 = 61%`
- Mob damage in = `21.5 × 1.05 ≈ 22.6` → `0.61 × 22.6 = 13.8 dmg/round`
- Warrior dies in: `(50×20+50) / 13.8 = ~76 rounds` (with my injected hp_max=1050)

**Warrior loses by ~120 rounds**. With *realistic* gear at L20, soloing trash is impossible. The mob math wins on both damage out (mob 13.8 vs player 2.2 → 6× advantage) and HP race.

## 6. Where does the game break, design-wise?

Pulling all of this together as a designer:

1. **Player damage output is the bottleneck.** Median tier-T2 weapon is 1d6-1d7 = 4-5 damage. Median apply-block damroll is ~1.4 (= +7 attack_power, +7% damage). That gives a typical L20 player ~4.5 damage per swing. Mobs have 100×+ HP. Players need either much bigger weapons, much higher attack_power scaling, or both.
2. **Mob accuracy has a structural edge.** Their `accuracy = 50 + hit_roll×2 + level×2` includes their authored hit_roll, which trash mobs commonly have at 20+. Players don't get an equivalent hit_roll baseline from their class — only from gear, which contributes ~1-3 hit_roll. So a same-level fight gives the mob a 60-70% hit rate vs the player's 50%.
3. **Armor mitigation is effectively zero** (§3d bug + apply-block direction). At every level, players take full damage. The mob damage curve at L20 (22 per hit), L40 (47), L50 (62) is unmitigated.
4. **The `level*2` baseline scaling in accuracy/evasion is symmetric** between player and mob, so it doesn't shift the balance — but it doesn't *fix* anything either. The asymmetry comes from mob hit_roll + weapon damage scaling.

## 7. Recommendations

Roughly ordered by impact:

1. **Fix §3d** (Objects.values.AC consumption). Single contained edit to `equip_apply.rs`. Restores armor mitigation immediately — expect L1-L20 solo to be reachable just from this fix once players have 4-6 armor pieces.
2. **Audit the apply-block AC direction.** Half the negative `target=ac` rows look like they came from items the legacy parser stored as positive values for *better* armor. If the sign-flip in the importer is double-flipping (or the original parser was inverting), fix that. After this fix the apply-block AC bonuses should also be positive on real armor.
3. **Class-based hit_roll scaling.** Add a per-level hit_roll progression to the `Class` table (`hit_roll_per_level`) and have `spawn_player` initialize accuracy as `50 + class.hit_roll_per_level × level × 2` plus gear deltas. This closes the structural hit-rate gap vs mobs.
4. **Weapon damage curve.** Tier T2 median damage of 5 isn't enough vs 400 HP mobs. Either (a) re-author the weapon drops to scale damage more aggressively per tier, or (b) introduce a stronger `attack_power` floor per class/level (warriors should land ~50% damage bonus from class alone at L20). (b) is less invasive.
5. **Re-survey after the fixes.** Once §1 lands, the curve table will reflect what gear actually contributes, and we can re-validate the L1-L20 solo target with a fresh sweep.

This bug **dominates everything else** in the combat-balance picture:
- AC values are unused → players take full uncut damage at every level
- Weapon HitRoll is in `Objects.values` but not in `CombatStats.accuracy` — so the avg HitRoll of ~0.69 in T2 is a moot point until the bridge is rebuilt
- Stat bonuses (`str_bonus`, `dex_bonus`, …) from items: all dead

The L15-stallion sweep loss is at least partly *because* my warrior had no armor mitigation despite "wielding" a longsword in the harness. The harness was setting `armor_pct` directly via `set_player_field`, but for a real player, gear contributes nothing.

## 4. Legacy comparison

`fierylib` import path is `~/Code/mud/lib/world/obj/*.obj` (141 zone files). For a sample of representative weapons across tiers, dump:
- legacy dice / hitroll / APPLY-block modifiers (CHAR_HITROLL, CHAR_DAMROLL, CHAR_AC_APPLIED)
- modern post-import values
- delta / drift

Verified for crude club (zone 10, id 1010): legacy `2d2 type=6`, modern `Hit Dice {num:2 size:2}` — identical. So the import preserves weapon dice; any divergence is in the *formula*, not the data.

## 5. Combat math vs mob curves (TODO — Iteration 2)

For each tier, compute the expected outcome of "TestWarrior level N + median-tier gear + injected hp_max" vs same-level trash. This is what the sweep is *trying* to measure but with realistic gear instead of my stat injection.

## 6. Verdict & recommendations (TODO — Iteration 3)

Pending data above.

---

## Process notes

- The data in §1-§2 is from the post-fix DB (mob evasion formula corrected, TestCleric seeded).
- Re-imports happen via `bash scripts/full_reset_and_import.sh` in `fierylib/`. Last full reset: 2026-05-12 (this session). The 2026-05-12 reset failed in `text_seeder` (unrelated `LoginStage.LOGIN_APPROVAL_PENDING` enum bug); world+mob data completed successfully before that crash.
- Sweep harness: `/tmp/sweep_clean.sh`, uses AdminChar to `purge` the arena between fights, parses room JSON via python3, detects LOSS via player `health.hp <= 0`.
