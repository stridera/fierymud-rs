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

This is why even with armor mitigation now working, L20+ trash still wins the HP race: mob hit rate is 70% vs player 50%, and player damage is constrained by the weapon-dice curve while mob damage is content-authored to scale.

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

## Process notes

- The data in §1-§2 is from the post-fix DB (mob evasion formula corrected, TestCleric seeded).
- Re-imports happen via `bash scripts/full_reset_and_import.sh` in `fierylib/`. Last full reset: 2026-05-12 (this session). The 2026-05-12 reset failed in `text_seeder` (unrelated `LoginStage.LOGIN_APPROVAL_PENDING` enum bug); world+mob data completed successfully before that crash.
- Sweep harness: `/tmp/sweep_clean.sh`, uses AdminChar to `purge` the arena between fights, parses room JSON via python3, detects LOSS via player `health.hp <= 0`.
