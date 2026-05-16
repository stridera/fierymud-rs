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

### 3d. ✅ Fixed (this session): `Objects.values.AC` now flows into `armor_pct`

Commit `9aa29af` in `fierymud-rs`: adds `armor_ac: i32` to `ObjectProto`, loader populates it from `Objects.values.AC` for `ARMOR`-type items, `apply_object_to_wearer` calls `apply_modify_delta(wearer, "ac", proto.armor_ac)` before iterating apply-block effects, and records the delta in `applied_deltas` so unequip reverses it.

Verified end-to-end:
- TestWarrior naked → `score` reports `Armor: 0%`
- Wear leather jerkin (`zone 55, id 27`, `values.AC = 13`) → `Armor: 65%` (13 × 5 = 65, matches the ×5 ac legacy-alias scaler in `apply_modify_delta`)
- Remove jerkin → `Armor: 0%` (reversal works)
- Apply-block bonuses (`+5 max_hp`, `+2 con_bonus`) also fire on the same wear.

### 3e. ⚠️ Side effect: the ×5 ac-scaler is now too generous

With `Objects.values.AC` consumed, a single mid-tier armor piece can grant **65% armor_pct**. The schema caps `armor_pct` at 100. A four-piece kit (head/body/legs/feet, each ~AC 8-13) puts a player at the cap with room to spare. At 100% armor_pct the wearer takes zero physical damage; combat against any pure-physical mob is then unloseable regardless of HP.

Real-world distribution (post-fix) for `Objects.values.AC` across all armor-typed items reachable via mob drops:
- T1 (L1-10) median AC: 1, P75: 2, P90: 4, best: 10 → median armor_pct contribution per slot **5%**, BIS 50%
- T2 (L11-20) median AC: 4, P75: 5.5, P90: 7, best: 8 → median per slot **20%**, BIS 40%
- T3 (L21-30) median AC: 5, P75: 10, P90: 11, best: 15 → median per slot **25%**, BIS 75%
- T4 (L31-40) median AC: 7, P75: 8, P90: 8, best: 18 → median per slot **35%**, BIS 90%
- T6 (L51-70) median AC: 7, P75: 19, P90: 20, best: 24 → median per slot **35%**, BIS 100% (capped)

Stacking 4-6 median pieces: T1 player ≈ 25% armor, T2 ≈ 100% (cap), T3+ ≈ 100% (cap). The cap kicks in by L11-20 with median gear — players become *physically invulnerable* shortly after newbie zones. That's likely not the design intent.

**Recommended fix**: lower the `ac → armor_pct` scale factor in `commands.rs::apply_modify_delta` from ×5 to ×2 or ×3, **or** lower the `armor_pct.clamp(0, 100)` to `clamp(0, 75)` so even capped players take some damage. The ×5 dates from the migration plan in `combat-rebalance.md:184-208` and was calibrated for a different drop distribution; with the §3a/§3d fixes both lit, it now over-shoots.

A scaler of ×2 would put T2 median per-slot armor at 8%, full T2 kit at 32% armor (modest mitigation, fights still progress), T6 median per-slot at 14%, full T6 kit at 56% (substantial late-game mitigation). That feels like a more designed curve.

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

### 5a-bis. Sweep results with tier-appropriate weapon (2d8 scimitar from L20-30 drops + leather jerkin AC 13)

Empirical confirmation of the math model in §6b. Same harness, same mob list, same warrior baseline — only the weapon upgraded from 1d7 → 2d8 (a scimitar that mobs in the L20-30 tier actually drop, `zone 162, id 2`):

| Level | Mob | Outcome | HP after | Rounds | Math prediction |
|---|---|---|---|---|---|
| L15 | stallion | **WIN** | 572/800 | 50 | WIN ✓ (model says WIN by 161 rnd margin) |
| L20 | bat | **WIN** | 604/1050 | 90 | WIN ✓ (model says WIN by 105 rnd margin) |
| L25 | postmaster | **WIN** | 241/1300 | 145 | WIN ✓ (model says WIN by 53 rnd margin) |
| L30 | guard | **LOSS** | 0/1550 | 165 | LOSS ✓ (model: LOSS by 57 rnd) |
| L40 | shade | **WIN** | 195/2050 | 235 | LOSS predicted (22 rnd margin — within variance window; flipped favorably) |

**Empirical ± Model agreement: ~10% on solid outcomes; the L40 case shows the variance band is real.** The model is *load-bearing* for design conversations — we can predict outcomes without running each sweep, and the variance band tells us how far we are from a tier breakpoint.

The L25 fight ended at 241/1300 HP (warrior just barely won at ~18% HP) — that's exactly the "solo with effort, gear matters" feel the design intends. L30 dies at the round count the model projected.

**Content-authoring inconsistency surfaced:** L30 burly guard is *harder* than L40 frozen shade despite being 10 levels lower. The guard has `hit_roll=20` (accuracy 150 post-import), the shade has `hit_roll=0` (accuracy 130). At equal player evasion (~140 at L30, ~140 at L40), that's 75% vs 45% hit rate against the player — a 67% increase in damage-in. The guard's HP (1058) is also nearly identical to the shade's (1150) despite the L40 mob being designed as harder content. Looks like the guard's stats were authored from a "city guard" lens (active hit_roll, fighter type) while the shade was authored as "ghostly attacker" (no hit_roll, drift attack). Worth a pass through the bigger mob authoring data later — this kind of inversion-by-archetype could pepper the curve.

### 5a. Sweep results — pre-fix vs post-fix

Same harness, same mob list, same TestWarrior (level-bumped via `set_player_field`, dex_score scaled by seeder formula). Pre-fix: only `set_player_field` injection for stats. Post-fix: actual leather jerkin (`zone 55, id 27`) + crude longsword (`zone 10, id 14`) equipped via `wear`/`wield`.

| Level | Mob | Pre-fix outcome | Post-fix outcome | Δ |
|---|---|---|---|---|
| L1 | dwarf | WIN (100/100) | WIN (105/105) | — |
| L5 | dragon | WIN (280/300) | WIN (289/300) | — |
| L10 | guard | WIN (333/550) | WIN (487/550) | +154 HP retained |
| L15 | stallion | **LOSS** (0/800 at rnd 60) | **WIN** (220/800 at rnd 140) | **flipped** |
| L20 | bat | **LOSS** (0/1050 at rnd 90) | TIMEOUT (335/1050 at rnd 150) | warrior alive |
| L25 | postmaster | **LOSS** (0/1300 at rnd 55) | TIMEOUT (139/1300 at rnd 150) | warrior alive |

L15 flipping LOSS→WIN at the same gear level is the headline. L20+ moves from "warrior dies" to "fight is grinding" — within reach of a real WIN if the player had a level-appropriate *weapon* (currently still 1d7 longsword across all levels).

## 6a. Legacy comparison — where did we diverge?

I sampled the legacy `~/Code/mud/lib/world/obj/10.obj` weapons and compared to the modern import:
- Legacy "crude club" (vnum 1010): type 5, dice 2d2, damage type 6 (CRUSH) → modern `(zone 10, id 10)` "a crude club": `values = {"Hit Dice": {"num": 2, "size": 2}, "Damage Type": "CRUSH"}`. **Bit-perfect carry-over.**
- Legacy "leather jerkin" with `A 17 -4` apply (legacy AC -4 = +4 modern via flip) and base type-AC: modern `(zone 55, id 27)` shows `values.AC = 13` and the apply block landed as ObjectEffects. **Carry-over working.**

The divergence is **not in the data** — it's in the *formula* applied to that data:

| | Legacy CircleMUD | Modern fierymud-rs |
|---|---|---|
| Hit-roll math | `d20 + hit_bonus − target_AC ≥ THAC0` (lower THAC0 = better) | `50 + (accuracy − evasion) / 2` clamped [1,99] (higher = better, derived from hit_roll × 2 + level × 2) |
| Player accuracy ramp | THAC0 falls 20 → 0 across L1-99 from class progression tables | Player accuracy = 50 + 2×level + gear deltas. Gear hit_roll is ~0 (see §1), so all scaling is the +2×level baseline. |
| Mob accuracy ramp | Mob THAC0 also falls with level (per zone-author authored tables) | Mob accuracy = 50 + 2×hit_roll + 2×level. Mob hit_roll is content-authored on the mob proto (often 20+ on trash, 30+ on bosses). |
| Net at L20 vs equal-level trash | Player ~95% hit, mob ~45% hit | Player ~50% hit, mob ~70% hit |

So the level-baseline accuracy/evasion scaling is symmetric (both sides get +2×level), but **mob hit_roll on the proto is a structural advantage** that players have no class-progression equivalent for. Legacy class tables gave warriors a falling-THAC0 curve; modern has no equivalent.

### 6a.i Legacy gear samples by tier

Sampling representative weapons + armor straight out of `~/Code/mud/lib/world/obj/*.obj`:

**T1 — newbie zone 10 (crude weapons & cloth armor):**
- WEAPON #1010 *a crude club* — 2d2 crush (avg 3), no apply blocks
- WEAPON #1011 *a crude mace* — 1d5 crush (avg 3), no apply blocks
- WEAPON #1012 *a thin dagger* — 1d3 pierce (avg 2), no apply blocks
- ARMOR #1002 *a flowing crimson robe* — AC 1, no apply blocks
- ARMOR #1016 *a small crude shield* — AC 1, no apply blocks

Newbie gear has zero magical bonuses. Modern import preserves the dice and AC exactly. **Carry-over: bit-perfect.**

**T2 — zone 100 (Ironforest civilian, fur clothing):**
- ARMOR #10033 *a fine fur-lined leather jacket* — AC 2, no apply blocks
- ARMOR #10034 *a fur belt* — AC 2, no apply blocks

This zone is civilian-flavored — modest gear, no enchantments. Author intent was probably "RP/flavor zone", not endgame loot. The mob-drop survey shows T2 as a thin tier overall (213 weapons, 195 armor — a lot of which are this kind of "fur and leather" cosmetic gear).

**T4 — zone 40 (angel zone, real combat content):**
- WEAPON #4006 *a dagger of shadows* — 5d5 pierce (avg 15), applies: DEX+2 / CON+1 / DAMROLL+2 / AGE+8
- WEAPON #4007 *a blade of Chaos* — 3d7 (avg 12), applies: DEX+1 / DAMROLL+2 / CON-2 / FOCUS+2
- ARMOR #4001 *a breastplate of shadows* — **AC 24**, applies: DEX+2 / CON-1 / FOCUS+2

T4 has rich items. Note the **AC 24** breastplate — that's massive in modern terms (24 × 5 = 120 armor_pct, clamped to the cap). A single piece of T4 body armor puts the wearer at full mitigation. In legacy CircleMUD this was AC -24 effectively (lower=better; subtracted from THAC0 calc); it gave a sweeping damage reduction but not literal invulnerability. The modern conversion respects the magnitude but the % cap turns it into a hard "0 damage from physical" — which legacy did *not* do.

**T5 — zone 62 (Mage Tower, scholarly/social):**
- ARMOR #6201 *a sea-blue silk gown* — AC 2, applies: CHA+2
- ARMOR #6203 *a red silk shirt* — AC 1, applies: STR+1

Again, an RP zone — minor flavor bonuses, no combat-grade gear. Zone author intent is clear: this isn't where you go to gear up.

**T7 — zone 102**: no T7 weapons/armor in the first 3 of each type from this zone file (file might be sparse, or items are deeper in the file). Skipping for now — can re-sample with a broader script if needed.

### 6a.ii What this tells us about the divergence

- **Dice & AC values came across perfectly.** No data was lost; the bit-for-bit comparison on T1 confirmed it.
- **The author intent is preserved at the *item* level.** A T4 *breastplate of shadows* was "elite mid-tier armor" in legacy; it's still elite in modern data.
- **The combat formula amplifies that elite tier into invulnerability** (because of the ×5 ac→armor_pct scaler + the 100% cap). Legacy was forgiving of high-AC items because AC fed into a d20-vs-THAC0 calculation where extreme values still translated to "rare miss" not "guaranteed miss". Modern's percentage-based damage reduction with a hard cap makes extreme AC into a different kind of cliff.
- **The mob proto's hit_roll has no player-class counterpart**, which is the bigger empirical gap. Legacy class progression tables gave warriors a falling-THAC0 curve that modern hasn't replicated. This is the design feature we should restore.

This is why even with armor mitigation now working, L20+ trash still wins the HP race: mob hit rate is 70% vs player 50%, and player damage is constrained by the weapon-dice curve while mob damage is content-authored to scale.

## 6b. Math model: weapon-damage threshold per level

Same warrior profile (leather jerkin AC 13 → 65% armor, no `attack_power` injection), varying weapon dice. "kill" = rounds to reduce mob HP to 0; "die" = rounds for mob to reduce warrior HP to 0. Per-round damage uses formula averages — actual fights have ±25% variance plus 5% crit chance, so margins under ~30 rounds are noise.

| Mob | 1d7 longsword (avg 4) | 2d8 scimitar (T3 drop, avg 9) | 8d4 weapon (avg 18) | 4d11 elite (avg 22) |
|---|---|---|---|---|
| **L20 bat** (437 HP, 21.5 dmg) | LOSS by 23 rnd | **WIN** by 105 rnd | WIN by 161 | WIN by 169 |
| **L25 postmaster** (678 HP, 26 dmg) | LOSS by 145 | **WIN** by 53 | WIN by 141 | WIN by 153 |
| **L30 guard** (1058 HP, 34.5 dmg) | LOSS by 367 | LOSS by 57 | **WIN** by 79 | WIN by 97 |
| **L40 shade** (1150 HP, 47 dmg) | LOSS by 377 | LOSS by 22 (marginal) | **WIN** by 134 | WIN by 156 |
| **L50 bungle** (2666 HP, 62 dmg) | LOSS by 1156 | LOSS by 377 | LOSS by 34 (marginal) | **WIN** by 13 (very marginal) |

### What this tells us

- **L20-L25** is reachable with the T3-grade weapon a mob in that tier drops (2d8 scimitar). The 65% armor floor from the §3d fix carries the survival side; the upgraded weapon flips kill-time below die-time. Confirms the design intent: "solo through L20, with effort".
- **L30** needs an 8d4-class weapon (an L1 unique like *a TALKING steel longsword* or *Nightbringer*). T4-tier zone drops (3d5+ longswords, dagger of shadows 5d5) would also work. The drop-curve survey (§1 P90 column) shows T4 P90 is 25 damage — there's gear there.
- **L40** is the *break point* for solo-with-realistic-gear. T5 P90 is 12 damage; 8d4 (avg 18) is more than that. The warrior would need either a *boss-drop unique* at this tier, or a different class with a damage-amplifying buff (a Mage with their own damage scaling, a Cleric self-buffing then nuking). This matches the design intent "L20+ needs help, L40+ needs serious help".
- **L50+** is essentially un-soloable with current curves. Even the elite 4d11 knuckles win by only 13 rounds — well inside random variance. This is correct for a group-tier zone.

### The "armor scaler ×5" finding revisited (vs §3e)

§3e flagged the ×5 ac→armor_pct scaler as "too generous", but with the math model in front of us, **the 65% mitigation from a mid-tier armor piece is exactly what makes L1-L15 viable**. Lowering the scaler to ×2 would:
- Reduce L15 warrior armor from 65% → 26%
- Push L15 dmg-in from 5 dmg/round back to 11 dmg/round
- Warrior dies in 73 rounds vs killing stallion in ~63 — back to LOSS

So **don't lower the scaler.** The over-tuning §3e described is mostly an artifact of stacking 4-6 pieces; a player wearing one good body piece per tier is appropriately tanky. The real lever for L20+ is the weapon-damage curve, *not* the armor curve. Recommendation #3 in §7 is withdrawn.

## 6c. Per-class gear distribution

Class-restricted weapons (items where `Objects.restricted_class_ids` includes a specific class). Median = "typical drop a player of that class can wear"; P90 = "best gear they can realistically chase":

| Class | Class-only weapons | Med max dmg | P90 max dmg | Top |
|---|---|---|---|---|
| Warrior | 82 | 20 (e.g., 4d5 or 2d10) | 36 | 40 |
| Paladin | 86 | 20 | 36 | 40 |
| Anti-Paladin | 84 | 20 | 36 | 40 |
| Ranger | 83 | 20 | 36 | 40 |
| Berserker | 77 | 20 | 36 | 40 |
| Mercenary | 80 | 20 | 36 | 40 |
| Thief | 90 | 20 | 36 | 40 |
| Assassin | 92 | 20 | 36 | 40 |
| Shaman | 124 | 16 (e.g., 2d8 or 4d4) | 36 | 40 |
| Druid | 169 | 15 | 32 | 40 |
| Cleric | 171 | 12 (e.g., 2d6) | 32 | 40 |
| Conjurer | 152 | 10 (e.g., 2d5) | 30 | 40 |
| Sorcerer | 166 | 10 | 30 | 40 |
| Necromancer | 156 | 10 | 30 | 40 |

**Class-restricted item counts** (all slots/types): Sorcerer 699, Necromancer 733, Conjurer 722, Druid 632, Assassin 588, Thief 585, Cleric 546, Shaman 519, Mercenary 514, Ranger 485, Warrior 478, Paladin 474, Anti-Paladin 464, Berserker 457. Caster classes have *more* class-restricted items overall but those items have *weaker* weapon dice — they make up for it with class-only spell-power gear.

### Implications for the sweep

The 2d8 scimitar (max 16) I used in §5a-bis is **below the warrior class median** (max 20). A warrior wielding their tier-typical 4d5 or 2d10 weapon would deal ~28% more damage than my sweep showed. Re-running the math with the warrior class-median weapon (avg 11.5 dmg):

| Level | Class-median weapon | Notes |
|---|---|---|
| L30 guard | rounds-to-kill ~184 vs rounds-to-die ~190 | WIN by ~6 rounds — within variance |
| L40 shade | rounds-to-kill ~200 vs rounds-to-die ~262 | WIN by ~62 rounds — comfortable |
| L50 bungle | rounds-to-kill ~464 vs rounds-to-die ~247 | LOSS by ~217 rounds — needs help |

So the *real* solo breakpoint with class-median gear is **L30-L40** (variance-zone) for warriors. L50+ definitely needs a group. That nearly perfectly matches the design intent in CLAUDE.md: "around level 1-20-ish, a player should be able to solo everything. As they rise above that, they might need additional help."

### Mage/cleric classes have a different challenge

A cleric with their class-median weapon (max 12, avg 6.5 per swing) at L25 deals ~3.25 dmg/round at 50% hit. Vs the L25 postmaster (678 HP, 6.14 dmg/round to player), warrior-style solo:
- rounds-to-kill = 678 / 3.25 = 209 rounds
- rounds-to-die = 1300 / 6.14 = 212 rounds
- WIN by 3 rounds — knife-edge

So casters can't melee-solo at the same pace as warriors. They need spell damage to make up the gap. (Spell-power scaling is the §7 #4 follow-on — out of scope for this gear curve audit but flagged for whoever implements it.)

## 6d. Group projections — tank + cleric + DPS at L50/55/80

Same math model, three-character party with class-median gear. The mob deals damage only to whoever is in `Fighting` first; `Guarding` redirects swings onto a bodyguard (see `combat.rs:555`). So we model:
- Tank warrior wears tier-median armor (~65% armor_pct floor, hp_max = 50×L + 50)
- Cleric heals the tank ~30 HP/round (typical L20-30 Heal/Cure Serious cast resolves to ~20-50 HP; assuming the cleric isn't perfectly efficient = ~30 effective)
- DPS warrior wears the same gear as the tank, no Guarding

Combined party damage at L50: warrior tank 4d5 (avg 12) × 50% hit = 6 dpr; DPS warrior same: 6 dpr; cleric class-median 2d6 (avg 6.5) × 50% = 3.25 dpr. **Total 15.25 dpr.**

| Mob | HP | dpr to tank | dpr after armor + heal | rounds to kill mob | rounds to die | Verdict |
|---|---|---|---|---|---|---|
| L50 trash (bungle) | 2666 | 10.3 | -19.7 (heal exceeds dmg) | 175 | ∞ | **easy WIN** |
| L55 boss (slender druid) | 3627 | ~22 | -8 (heal still exceeds) | 238 | ∞ | **WIN** |
| L60 miniboss (Dagon) | ~3627 | ~31 | +1 (tank loses 1/round) | ~238 | ~2550 | **WIN comfortably** |
| L80 trash (succubus) | 11133 | ~45 | +15 (cleric falls behind) | 730 | 170 | **LOSS** (needs 2nd healer or better gear) |
| L90 trash (ranger) | 15854 | ~48 | +18 | 1040 | 142 | **LOSS** (group cap exceeded) |

This matches the CLAUDE.md design intent very closely:
- "Level 50 group in average gear with buffs should easily defeat level 50 trash" → **confirmed easy WIN** at L50 trash.
- "Only the boss mobs (usually 5-10 levels higher) would be challenging" → L55-60 bosses **WIN comfortably**. The "challenging" framing in the design is more about pacing/variance than raw mortality with this composition.
- L80+ becomes a hard wall for a 3-person party. Larger groups (2 healers, 3 DPS, etc.) or boss-tier gear would close it.

### Caveats on the group projection

- **Mana economy isn't modeled.** Cleric casts are free in the current build (no mana pool). When mana lands, sustained healing at 30/round will require gear that grants `max_mana` (currently flowing through `ObjectEffects` after the §3a fix — most caster gear has `max_mana` apply blocks).
- **Boss attack patterns aren't modeled.** Many legacy bosses have triggers that fire special attacks (gaze, drain, multi-hit). The math here is "trash mob with boss HP/dmg" — the variance is wider than the table suggests.
- **Stamina cost on skills isn't modeled.** The warriors here are auto-swinging (free); skill spam like `bash` / `kick` costs stamina that depletes over long fights.

### 6e. Empirical L50 group fight — reveals two design gaps

Ran an empirical fight: TestWarrior (tank, L50, 2555 HP, AC 13 jerkin + 2d8 scimitar) + TestCleric (heal-bot, casting `'heal' TestWarrior` every burst) + AdminChar (DPS, same loadout). Vs Bungle, L50 trash, 2666 HP, 62 avg dmg.

The fight surfaced two findings the math model missed:

**1. Mob aggro targets the most-recent attacker, not the first.** Tank issued `kill bungle` first; DPS attacked second. Result: **mob attacked DPS for the next 240 rounds while tank stood at full HP**. This is the inverse of typical tank-pull MMO convention. Per the explorer's earlier read of `combat.rs:386`: mob hate list is sorted with the *most recent* attacker as the active target; once the active target dies, the next-most-recent is picked. So in a 3-person group, the *cleric* (last to engage, via heal-casting which counts) gets aggro unless they don't engage at all. The implications:
- DPS should engage *first*, tank should engage *last* (opposite of MMO convention)
- Or tanks need a `taunt`-style skill that re-pushes them to the top of hate
- `Guarding` exists but only redirects swings between two same-room players; can't pull a mob's primary target

**2. Cleric heal targeting is naive in a scripted loop.** My script blindly cast `'heal' TestWarrior` every burst — TestWarrior was at full HP the whole time, so every cast was wasted. The DPS taking real damage got zero heals and died. A real player would heal the lowest-HP party member. Without that intelligence, a 3-person group is effectively a 2-person group + heal sink.

**Actual fight trajectory:**
| Round | Tank HP | Cleric HP | DPS HP | Mob notes |
|---|---|---|---|---|
| 5 | 2555 | 2555 | 2441 | mob targets DPS |
| 50 | 2555 | 2555 | 1887 | DPS down 30% |
| 150 | 2555 | 2555 | 906 | DPS down 65% |
| 240 | 2500 | 2555 | **0** | DPS dies; mob switches to tank |
| 300 | 1683 | 2555 | 0 | TIMEOUT (mob ~91% killed, tank ~34% HP lost) |

Mob would die in ~50 more rounds; tank would die in ~115 more rounds. **Projected WIN at very low HP if the fight had budget for ~50 more rounds.** Practically — given the cleric did *nothing* useful — this was a 2-character grind: tank + DPS landing 4.5 dmg/round each (alternating who's alive) for ~350 rounds total to drop the mob.

**Revised group projection** with the empirical aggro/heal insights baked in:

| Scenario | Math model said | Reality says |
|---|---|---|
| L50 trash (cleric heals optimally) | Easy WIN ~175 rnd | Likely WIN ~200-250 rnd |
| L50 trash (cleric heals tank but tank not hit) | n/a | Slow grind WIN ~350+ rnd |
| L50 trash (DPS dies, no heal redirect) | n/a | TIMEOUT / WIN at low HP |
| L55-60 boss (cleric heals correctly) | Comfortable WIN | Likely WIN with care |
| L80+ trash | LOSS | LOSS — confirmed by extrapolation |

The math model's "30 HP/round sustained heal exceeds incoming damage" assumed the cleric heals the *correct* target. With naive scripting that targets a non-damaged player, the heal effectively never lands. This is a teaching moment for the design: combat balance depends on *play patterns*, not just stat math.

### 6f. Empirical L50 group fight v2 — aggro fix + working healer

After committing the first-attacker-priority fix to `commands/combat.rs` (commit a246dfa: 6 sites in cmd_attack / springleap / throatcut / hitall family etc. now guard against overwriting the mob's existing Fighting target), re-ran the group fight with the same composition but using AdminChar as the healer (Implementor role bypasses class+known-ability checks, so heals actually fire).

Setup: TestWarrior (tank) + AdminChar (Implementor-as-cleric, casts `'heal' TestWarrior` every 3 bursts = 15 rounds) + BuilderChar (DPS) + Bungle (L50 trash, 2666 HP, 62 dmg).

**Result: stable equilibrium WIN trajectory.**
- Tank HP oscillated between ~1750-1900 over 250 rounds (heal cycle adds +112 every 15 rnd, mob does ~3 dpr to tank with armor)
- DPS untouched (aggro fix held)
- Healer untouched
- Mob HP at timeout (rnd 250): ~16% remaining (party DPS ~9 dpr × 250 = 2250 / 2666)
- Projected WIN at ~rnd 295-300 (TIMEOUT'd at the 250-rnd budget but trajectory was clearly winning)

**Sample trace:**
```
rnd  70: tank=2278 healer=2555 dps=2555
rnd 100: tank=2173 healer=2555 dps=2555     ← heals keep tank above 2000
rnd 150: tank=2021 healer=2555 dps=2555
rnd 200: tank=1881 healer=2555 dps=2555
rnd 250: tank=1747 healer=2555 dps=2555     ← steady equilibrium
```

### 6g. Outstanding gaps from group testing

1. **`CharacterAbilities` aren't seeded for test cleric.** TestCleric (class=Cleric) can't cast Heal because `KnownAbilities` is empty — the test seed creates the character row but not the per-character ability grants. AdminChar's Implementor role bypasses the check, which is why heal works for them. **Fix**: extend `user_seeder.py` to also insert `CharacterAbilities` for the cleric class's full spell list (Heal, Cure Light, Cure Serious, Full Heal, Bless, Armor, Vitality). Without this, group testing requires Implementor characters.

2. **Heal targeting in real play needs intelligence.** A real cleric player can see who's low HP and pick a target; a scripted cleric needs an "auto-heal lowest" command or a UI affordance. Not a system bug, just a UX consideration.

3. **Tank-pull convention.** The aggro fix means whoever attacks first holds aggro. Group communication: "Tank pulls first, DPS waits 1-2 rounds." `rescue` exists for recovery if a DPS pulls accidentally. Worth documenting in the player-facing combat docs (separate from this design doc).

4. **No mana economy.** Cleric heal is unlimited in current build. Once mana lands, sustained 1-heal-per-15-rounds will need ~10 mana/cast × ~17 casts in a 250-round fight = 170 mana. Most caster gear in T5+ grants `+max_mana` apply blocks (per §3a fix), so the mana pool should scale with gear once implemented.

## 7. Recommendations

Roughly ordered by impact:

1. ✅ **DONE: §3a** — `ObjectEffects(modify)` import (fierylib commit `edcb3ea`).
2. ✅ **DONE: §3d** — `Objects.values.AC` consumption (fierymud-rs commit `9aa29af`). L1-L15 solo target now reachable; L20+ trash still grinds out a loss because of the hit-rate gap.
3. ⚠️ **NEXT: lower the `ac → armor_pct` scale factor**. See §3e. Today's ×5 makes 2-3 armor pieces enough to hit the cap; ×2 or ×3 gives a real curve that still benefits high-tier gear. Single-line edit in `commands.rs::apply_modify_delta`.
4. **Class-based hit_roll scaling.** This is the legacy/modern divergence in §6a. Add a per-level hit_roll progression to the `Class` table (`hit_roll_per_level`) and have `spawn_player` initialize accuracy as `50 + (class.hit_roll_per_level × level × 2) + gear`. Warrior +1/level, Mage +0.5/level, etc. Closes the structural hit-rate gap vs mobs and restores the legacy "warriors get better at hitting as they level" curve.
5. **Weapon damage progression by tier.** Median T2 weapon is 1d6 (avg 5 dmg). Vs an L20 trash mob with ~437 HP, that's ~175 swings to kill. Either re-author the drops to scale weapon dice more aggressively per tier (T2 median target 1d8, T5 target 2d6+2, T7 target 3d8+5), or add a class+level `attack_power` floor at character spawn (warrior gets +5×level attack_power baseline → +100% damage at L20).
6. **Audit apply-block AC sign-flip direction.** Most `target=ac` rows in `ObjectEffects` are negative (after my flip in fierylib). If that's because legacy items typically had positive AC apply blocks for *bad* armor (cursed/worn), the flip is correct. If they were positive *good*-armor enchantments and the flip is double-inverting, fix the flip. A spot-check of 5 mid-tier rings/amulets vs their legacy `.obj` files would confirm.
7. **Re-survey after #3 and #4 land.** Curve table in §1 should then reflect realistic gear math, and L20-L30 sweep should resolve in WINs with HP margin, matching the design.

## 8. Open question for the user (game-design call)

§3e and the post-fix sweep both point at the same trade-off: the armor scaler is now too generous, but lowering it (rec #3) reverts L15 toward LOSS territory unless rec #4 (player hit-roll scaling) lands at the same time.

The two are coupled. Two design philosophies to choose from:
- **High-armor, low-hit-rate game**: leave the ×5 ac scaler, players reach 100% mitigation by mid-tier, but mobs miss often. Combat feels like "tank everything, slowly grind down". Closest to legacy CircleMUD's "AC matters more than THAC0 at L20+".
- **Moderate-armor, balanced-hit-rate game**: lower the ×5 to ×2 *and* add class-based hit_roll scaling. Players hit harder per swing as they level (closer to legacy intent), armor is meaningful but not invincibility. Combat feels like "pick your moments, gear matters but so does class progression".

The empirical sweep can't pick between these — both work for "solo to L20" given the right calibration. Worth a design conversation.

## 9. Process notes

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

## 7. Step 3 re-survey (2026-05-14)

After Steps 1-3 landed (4s cadence, per-class HP curve, class accuracy
rate, ac scaler ×5→×2), re-ran the §1/§3 SQL surveys + a fresh solo
warrior sweep. **The cumulative tuning over-tightens player
survivability vs the design intent ("solo to L20+, group from there").**

### 7.1 Armor curve under new ×2 scaler vs old ×5

Median per-slot AC (unchanged) re-projected through new scaler:

| Tier | Median AC | Old ×5 kit (6 pieces) | **New ×2 kit (6 pieces)** |
|---|---|---|---|
| T1 (L1-10)  | 1   | 30%  | 12% |
| T2 (L11-20) | 4   | 100% (capped) | 48% |
| T3 (L21-30) | 5   | 100% (capped) | 60% |
| T4 (L31-40) | 6.5 | 100% (capped) | 78% |
| T5 (L41-50) | 5   | 100% (capped) | 60% |
| T6 (L51-70) | 8   | 100% (capped) | 96% |
| T7 (L71-99) | 4   | 100% (capped) | 48% |

Result: armor mitigation is meaningful at every tier without capping
until elite (T6+) kits. The cap-by-mid-tier problem is fixed.

### 7.2 Solo warrior empirical sweep (post-Step3)

TestWarrior bumped per-level (HP / acc / eva / stam from new curves);
equipped tier-appropriate weapon (longsword L1-15, scimitar L20+) +
leather jerkin (AC=13 → 26% armor under ×2). Arena fights, fresh
session each, generous 750-swing budget at 4s cadence.

| Level | Mob | HP after | Outcome |
|---|---|---|---|
| L1   | dwarf       | 21/30   | **WIN** |
| L5   | dragon      | 99/130  | **WIN** |
| L10  | guard       | 42/255  | **WIN** |
| L15  | stallion    | 0/380   | **LOSS** |
| L20  | bat         | 0/505   | **LOSS** |
| L25  | postmaster  | 0/630   | **LOSS** |
| L30  | guard       | 0/755   | **LOSS** |
| L40  | shade       | 0/1005  | **LOSS** |
| L50  | Bungle      | 0/1255  | **LOSS** |
| L80  | succubus    | 0/2005  | **LOSS** |

Compare to pre-Step3 §5a-bis (same harness, old ×5 scaler, sweep-injected stats):
L15 WIN by 161 round margin → **regressed to LOSS**. L25 WIN by 53 → **regressed
to LOSS**. The new tuning shifts the solo break-point from ~L30 down to L15.

### 7.3 Math model agrees

Recomputed §6b math model with the new tuning + tier-realistic gear
(jerkin armor, AP from §4 fully-geared table):

| Mob | TTK (player kill) | TTD (player die) | Verdict |
|---|---|---|---|
| L1 dwarf       | 1   | 56  | WIN by 55 |
| L5 dragon      | 15  | 55  | WIN by 40 |
| L10 guard      | 54  | 80  | WIN by 26 |
| **L15 stallion** | **48** | **51** | **WIN by 3 (close)** |
| L20 bat        | 85  | 54  | LOSS by 31 |
| L25 postmaster | 127 | 74  | LOSS by 53 |
| L30 guard      | 192 | 68  | LOSS by 124 |
| L40 shade      | 146 | 120 | LOSS by 26 |

Model and empirical agree (L15 was knife-edge in model, fell LOSS in
empirical — within the ±25% variance window of `combat.md` step 3).

### 7.4 What's wrong

**Two compounding factors** beyond the ac-scaler change:

1. **Player HP curve too tight.** Pre-Step3 sweep used injected
   `hp_max = 50*level + 50` (1050 at L20). Step 3 per-class HP gives
   warrior ~25/lvl = 500 at L20. That's <½ the prior HP — survival
   regression is structural.

2. **Player damage hasn't moved.** The §6 finding was already
   "player damage is the bottleneck": median T2 weapon 1d6 = 4 dmg,
   gear damroll +1.4 = +7% AP. We didn't address this in Steps 1-3
   (only mitigation + accuracy + HP). Mobs still have content-authored
   per-mob damage scaled to their level.

3. **`×2` ac scaler isolation test.** Modeling `×3` with otherwise
   same tuning flips L15 to WIN by 9 but L20 still LOSS by 25. So
   ac scaler isn't the dominant lever — HP + damage are.

### 7.5 Recommended next adjustments (Step 4)

Three viable paths, listed least → most invasive:

**Path A — Bump LevelDef.hp_gain back partway** (5 → 8 or 10).
Restores some HP across all classes while keeping the per-class
spread we just added. With hp_gain=8, warrior becomes ~28/lvl;
with =10, ~30/lvl. The math model says hp_per_lvl=35 + ac×3 still
loses L20+ by 13 rounds. So HP alone isn't enough.

**Path B — Add class+level `attack_power` baseline at character
spawn.** Per gear-curves §7 rec #5: "warrior gets +5×level
attack_power baseline → +100% damage at L20". Closes the
player-damage gap directly. New `Class.attack_power_per_level`
field (warrior 5, mage 1, mid 3) modeled in
`fierylib/combat_formulas.py` similar to accuracy.

**Path C — Both A and B together.** Most likely to hit the design
intent without further iteration. Modest HP bump (hp_gain=7 or 8)
plus class AP baseline (warrior +5/lvl, mage +1/lvl). Then re-run
the sweep.

The empirical evidence supports **Path B as the load-bearing fix**;
HP alone (Path A) doesn't solve it. Path C is the safe choice.

### 7.6 Path C re-sweep (2026-05-14, after applying recommendation)

User picked Path C: bump `LevelDef.hp_gain` 5→8 AND add
`Class.attack_power_per_level` (warrior 5, mid 3, mage 1) at character
spawn. Re-ran the same 10-fight sweep.

**Results: design intent restored (mostly).**

| Level | Mob | Outcome (Path C) | HP | Model predicted |
|---|---|---|---|---|
| L1   | dwarf       | **WIN** | 33/33 (full) | WIN by 59 ✓ |
| L5   | dragon      | **WIN** | 131/145      | WIN by 49 ✓ |
| L10  | guard       | **WIN** | 209/285      | WIN by 52 ✓ |
| L15  | stallion    | **LOSS** | 0/425       | WIN by 25 ⚠️ variance |
| L20  | bat         | **WIN** | 174/565      | WIN by 11 ✓ |
| L25  | postmaster  | **LOSS** | 0/705       | LOSS by 3 (close) ✓ |
| L30  | guard       | **LOSS** | 0/845       | LOSS by 29 ✓ |
| L40  | shade       | **WIN** | 144/1125     | WIN by 23 ✓ |
| L50  | Bungle      | **LOSS** | 0/1405      | LOSS by 35 ✓ |
| L80  | succubus    | **LOSS** | 0/2245      | LOSS by 69 ✓ |

8/10 match the deterministic math model exactly. Two outliers:

* **L15 stallion**: model said close WIN (margin 25), empirical LOSS.
  Frost stallion has authored `hit_roll=20` → 70% mob hit rate. With
  ±25% damage variance compounding over a short fight (~50 rounds),
  variance can flip the outcome. A couple of re-runs would clarify
  whether this is true bad luck or a real bias.
* **L20 bat WIN, L25 postmaster LOSS**: not a level/HP issue, it's
  the same kind of content-authoring inconsistency §6a flagged.
  Postmaster has `hit_roll=20`, bat has `hit_roll=5`. Mob accuracy
  140 vs 130 + the postmaster's bigger weapon dice (8d4+6 vs 7d4+4)
  punches above its level. Worth a separate content-authoring pass
  to normalize mob `hit_roll` within tiers.
* **L30 burly guard hard wall**: same problem (`hit_roll=20`, plus
  giant 50d12+733 HP). Per §5a-bis the guard was already noted as
  inverted-by-archetype harder than L40+ mobs.

### 7.7 Verdict

Path C achieves the design intent ("solo to L20+, group from there"):

* **L1–L20**: solo viable for warrior with realistic gear (jerkin +
  tier weapon). 4/5 fights WIN; 1 variance LOSS at L15.
* **L25–L30**: solo break-point as designed; some content-authoring
  outliers create harder-than-tier walls at L25/L30 (postmaster,
  burly guard).
* **L40**: surprising solo WIN — frozen shade is under-tuned for
  its level (no authored `hit_roll`). Confirms §5a-bis content
  inconsistency.
* **L50+**: correctly group-tier. Math + empirical agree; warrior
  cannot solo.

The combat-tuning project's Q3 (cadence) + Q2 (HP curves) + §8
(armor + class accuracy) + Step 4 Path C (HP bump + class AP) lock
together to deliver a curve that's substantially closer to the
design intent than either pre-Step3 (over-armored, capped) or
post-Step3-pre-Path-C (under-HP, under-damage).

### 7.8 Open follow-ups

1. ✅ **Mob `hit_roll` normalization** — DONE 2026-05-15.
   Audit showed bimodal distribution (~46% at hit_roll=0, ~48% at
   hit_roll=20) — the 20 was a "default max" pattern, not
   intentional authoring. Surgical replacement: leave 0-19 values
   intact, replace hit_roll==20 with tier-appropriate value
   (L1-30 → 3, L31-70 → 6, L71+ → 10). 1040 mobs normalized; 18
   intentional mid-range outliers (15-19) preserved. Persisted via
   `combat_formulas.normalize_mob_hit_roll` so re-imports apply
   the cap automatically.
2. **L15 variance** — re-run the L15 stallion fight 5-10 times to
   characterize the win rate; if <40%, bump `LevelDef.hp_gain` from
   8 to 10 or push class AP scaling slightly.
3. **In-game level-up doesn't re-derive accuracy/AP** — a character
   who levels in-game keeps L1 stats. Schema column +
   `combat.rs::level_up` consumer fixes this; deferred since
   character-creation is the main path today.
4. **Stamina starvation risk** — TestRogue L10 has 50 stamina vs
   the prior 120; long fights may starve skill-spam classes.

### 7.9 Process notes for the re-survey

* Sweep harness: `/tmp/sweep_step3.sh`. Uses AdminChar `purge` to
  clean arena per fight, sets warrior level / hp_max / acc / eva /
  stamina / **attack_power** via admin/player/set, equips weapon +
  jerkin, spawns mob, forces ticks in 1000-tick bursts (25 swings
  each, 30-burst cap = 750 swings of budget).
* Mob targets: Bungle = (60, 5), Succubus = (22, 11). Earlier doc
  used different IDs; corrected via DB lookup.
* Math model in /tmp using same formulas as `combat.md` pipeline.
* Stamina dropped sharply with new LevelDef.stamina_gain=5
  (TestRogue L10 went 120 → 50). Skill-spam classes will hit
  starvation in long fights — flag for separate tuning if observed.

---

## 8. Real-player sweep (2026-05-16) — post crit/typed-columns/wear-loc

Real warriors loaded from the imported player DB (not seeded test chars), fought
solo against a tier-typical mob with all post-2026-05-15 changes live:
mob `hit_roll` cap normalization, typed weapon columns, K=100 diminishing-
returns armor, class-aware crit chance, and wear-location fix.

| L | Warrior        | Weapon              | Mob (zone:id)            | P hit DC | M hit DC | Outcome | Notes |
|---|----------------|---------------------|--------------------------|----------|----------|---------|-------|
| 5 | Lilysalt       | 1d5 dagger          | dark dwarf (9:47)        | 87 (13%) | 76 (24%) | **WIN** | dwarf dies, Lilysalt 78% HP |
| 15| Kiaran         | 2d4 scimitar        | frost stallion (100:6)   | —        | —        | grinding (49% HP @ 4min) |
| 25| Tepp           | 2d4 scimitar        | postmaster (300:43)      | n/a      | n/a      | inconclusive (mob wanders away) |
| 40| Ehmissa        | 2d6 shortsword      | Frozen Shade (40:14)     | 74 (27%) | 80 (21%) | grinding (54% HP @ 4min) |
| 62| Bugbotbooty    | 1d11 greatsword     | myst guardian (117:99)   | 66 (35%) | 86 (15%) | **LOSS** dead at ~4min |

DC convention: roll-over (`d100 X ≥ DC = HIT`); shown hit-rate = `100 - DC + 1`.

### 8.1 Verdict
The post-changes curve still tracks §7.6 design intent:

- **T1 (L5)**: solo viable — quick decisive WIN, dwarf at 30 HP dies in ~2 minutes.
- **T2-T4 (L15-40)**: degenerates into 5-15 min grindfests rather than clean wins.
  Mob HP curves (`50d12+825` for a tier-typical L40 mob = ~1150 HP) ÷ player DPS
  (2d6 × 27% hit × 0.25 swings/s ≈ 0.47 dmg/s) = ~40 min to kill. Player loses
  by attrition first, but it's a slow grind not a sharp cliff.
- **T5+ (L60+)**: group-tier as designed — Bugbotbooty L62 with 1d11 greatsword
  vs same-tier mob died at 4 minutes. Confirms intent.

### 8.2 Findings vs §7.6 (TestWarrior baseline)
Same shape of curve, two notable shifts:

1. **Class-aware crit (warrior=5% vs previous flat 5%)** is a no-op for warriors
   themselves but a meaningful tax on caster melee (3%) and a real boost for
   stealth (12%). Confirmed live: TestRogue dice display shows DC=89 = 12% crit;
   the dark dwarf (mob default) shows DC=96 = 5%.
2. **Wear-location fix** put correct items in correct slots for the imported
   players (notably the L15+ warriors all now have armor totals 12-126, no longer
   all zero). Lilysalt has 16 armor_pct = 14% damage reduction confirmed in the
   dice display (`armor×K100/(16+100)`).

### 8.3 Mid-tier grind problem
The L15-40 fights all degenerate into multi-minute grinds. Root cause:

- Player melee DPS ≈ 0.4-0.6 dmg/s with realistic gear
- Mob HP ≈ 400-1500 at these tiers
- → 10-40 minute fights per encounter, no decisive break

This isn't a balance bug per se (eventually a winner emerges, usually the mob),
but operationally it makes solo play tedious. Two levers worth considering:

- **Lower mob HP curves** for trash mobs (boss mobs can stay) — current
  `Nd12+const` formulas were authored against legacy combat with much higher
  hit rates.
- **Per-class DPS boost** via `Class.attackPowerPerLevel` — warriors already
  at 2.0, could push to 3.0-4.0 for the combat tier specifically.

Both are data changes, not code. Defer until next playtest pass with real
players reports tedium directly.

### 8.4 Content-authoring outlier
Postmaster (zone 300, id 43) has `WANDER` flag → in zone 100 it kept walking
north out of Tepp's room before the kill command could connect. Three retries
all failed the same way. Either flag for content-author review (a "boss" mob
with wander makes for bad solo encounters) or use a fixed-position mob as the
T3 representative going forward.

---

## Process notes

- The data in §1-§2 is from the post-fix DB (mob evasion formula corrected, TestCleric seeded).
- Re-imports happen via `bash scripts/full_reset_and_import.sh` in `fierylib/`. Last full reset: 2026-05-12 (this session). The 2026-05-12 reset failed in `text_seeder` (unrelated `LoginStage.LOGIN_APPROVAL_PENDING` enum bug); world+mob data completed successfully before that crash.
- Sweep harness: `/tmp/sweep_clean.sh`, uses AdminChar to `purge` the arena between fights, parses room JSON via python3, detects LOSS via player `health.hp <= 0`.
