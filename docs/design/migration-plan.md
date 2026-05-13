# Schema / Loader Migration Plan

**Generated:** 2026-05-12
**Status:** all waves complete. Wave 7 (post-everything sweep) closed. Remaining open items live in [`parking-lot.md`](./parking-lot.md).
**Companion docs:** [`database-audit.md`](./database-audit.md) (the per-field rationale), [`parking-lot.md`](./parking-lot.md) (items needing user feedback later).

## Decisions (locked, 2026-05-12)

User answered all four scoping questions with "do it all":

| Question | Decision |
|---|---|
| Combat redesign | **Do it all now** — add Characters combat cols, migrate `ObjectAffects` → `ObjectEffects`+`ObjectResistance`, drop legacy `hit_roll`/`damage_roll`/`armor_class`/`*_dice_*`. |
| Account-level features | **Wire both** — `AccountItems` (shared chest) + `Users.account_wealth` (shared bank). |
| Federated identity | **Wire all** — `DiscordLink` + `GoogleLink` + `DiscordConfig` + per-character name approval (replaces the dropped `LoginRequests` design). |
| Quest depth | **Wire everything** — dialogue trees, `CUSTOM_LUA`, choice rewards, time/cooldown limits, exclusive groups, availability formulas, all trigger types. |

Strategy principles:
1. **Parity** with legacy FieryMUD is the floor.
2. **Improvements** allowed if they don't break parity.
3. **Wire** rather than drop unless not-used + not-parity + not-improvement.
4. **Tests** as the wiring proof.

## Coordination model

- **fierylib** owns Prisma schema changes (`muditor/packages/db/prisma/schema.prisma`) + Python importer.
- **fierymud-rs/mud-db** owns the SQL loader layer (`crates/mud-db/src/*.rs`).
- **fierymud-rs/mud-world** owns the ECS loader (`crates/mud-world/src/loader.rs`).
- **fierymud-rs/mud-server** owns the consumers (commands, combat, regen, etc.).

For drops: rs loader must stop SELECTing the column *before* the schema drops it.
For wires: rs loader can start SELECTing only *after* the column exists.

## Wave 1 — Safe drops (no loader breakage)

These tables/columns are not loaded by `mud-db` and have no rs consumer. Drop in `schema.prisma` + run `bun run db:push`.

| # | Drop | Status |
|---|---|---|
| 1.1 | Table `MobCarrying` | ✅ dropped |
| 1.2 | Tables `EquipmentSets` + `MobEquipmentSets` + `EquipmentSetItems` | ✅ dropped |
| 1.3 | Tables `ShopHours` + `ShopRooms` + `ShopAbilities` | ✅ dropped |
| 1.4 | Table `PlayerToggle` | ✅ dropped |
| 1.5 | Tables `ShapechangeForm` + `AbilityShapechangeForm` | ✅ dropped |
| 1.6 | Tables `DeploymentPackage` + `DeploymentChange` | ✅ dropped |
| 1.7 | Table `SpawnConditions` | ✅ dropped |
| 1.8 | Table `PlayerMail` (we use AccountMail only) | ✅ dropped |
| 1.9 | Cols `Triggers.{mobZoneId, mobId, objectZoneId, objectId, variables}` | ✅ dropped |
| 1.10 | Cols `Characters.{raceType, playerClass, baseHeight, baseWeight, baseSize, currentSize, averageStats}` | ✅ dropped |
| 1.11 | Col `Mobs.averageStats` | ✅ dropped |
| 1.12 | Col `Ability.classes` (JSON dup of ClassAbilities) | ✅ dropped |
| 1.13 | Enum `Composition` + col `Mobs.composition` + col `Races.defaultComposition` | ✅ dropped. Legacy "stone golem immune to pierce" replaced by `resistances` JSON. |
| 1.14 | Enum `MagicAffinity` + col `Room.magicAffinity` | ✅ dropped |
| 1.15 | Enum `Stance` + col `Mobs.stance` | ✅ dropped |
| 1.16 | Col `Room.requiredMechanic` (keep PositionMechanic enum — PositionData uses it) | ✅ dropped |
| 1.17 | Cols `Mobs.{estimatedHp, raceAlign, position}` | ✅ dropped — replaced by protectedKind / defaultPosition |
| 1.18 | Col `Characters.is_online` | ✅ dropped — presence inferred from session |
| 1.19 | Col `Characters.birth_time` | ✅ dropped — `created_at` suffices |
| 1.20 | Col `Characters.wiz_title` | ✅ dropped — `title` covers it |
| 1.21 | Col `Characters.auto_invis_level` | ✅ dropped |
| 1.22 | Col `Characters.page_length` | ✅ dropped — runtime defaults pagers |
| 1.23 | Col `Characters.olc_zones` | ✅ dropped — `permissions` covers it |
| 1.24 | Col `AccountMail.is_broadcast` | ✅ dropped — broadcast inferable from null recipient |
| 1.25 | Col `Effect.param_schema` | ✅ dropped — Muditor-only validation; not runtime |
| 1.26 | Cols `Shops.{temper, noSuchItemMessages, doNotBuyMessages, missingCashMessages, buyMessages, sellMessages}` | wired — see Wave 2.AA |

## Wave 2 — Wire latent schema (parallel-dispatchable)

Each wire unit: extend `mud-db` loader → wire consumer in `mud-server`/`mud-world` → add a test.

| # | Wire | Notes |
|---|---|---|
| 2.A | `Object.flags` (ObjectFlag enum) | GLOW/HUM/INVISIBLE/MAGIC/PERMANENT/TEMPORARY/DECOMPOSING/FLOAT/BUOYANT/VEHICLE/SOULBOUND. Legacy parity. Consume in look/identify/equip/drop. |
| 2.B | `Object.restrictions` (ObjectRestriction enum) | NO_DROP/NO_TAKE/NO_SELL/NO_BURN/NO_LOCATE/NO_INVISIBLE. Legacy parity. |
| 2.C | `Object.{timer, decompose_timer}` | Item decay tick. Legacy parity. |
| 2.D | `Object.{allowed_races, min_size, max_size}` | Inclusive equip restrictions. Partial parity. |
| 2.E | `Object.{fixture_room_zone_id, fixture_room_id}` | Permanent room fixtures. Improvement. |
| 2.F | `Object.passenger_capacity` | VEHICLE riders. Partial parity. |
| 2.G | `Object.presence_override` | Position presence override (flying carpet). Improvement. |
| 2.H | `Object.{notes, tags}` | Builder search metadata. Improvement, cheap. |
| 2.I | Room booleans (`allows_magic/recall/summon/teleport`, `is_death_trap`, `is_indoors`, `is_soundproof`, `is_arena`, `is_guildhall`, `allows_mobs/tracking/portals/scanning`) | All legacy parity (ROOM_NOMAGIC/DEATH/INDOORS/etc.). |
| 2.J | `Room.entry_restriction` (Lua) | Improvement; gates entry by Lua eval. |
| 2.K | `RoomExit.hit_points` (bashable doors) | Legacy parity (bash skill). |
| 2.L | Mob latent (composition is being dropped; instead wire `size`, `lifeForce`, `damageType`, `move`, `defaultPosition`, `traits`, `movementMode`, `defaultMovementMode`) | ✅ 2026-05-12 — eight columns load into `Mob` + `MobProto`; spawn paths (loader / respawn / summon) attach `Sized` / `LifeForceTag` / `NaturalAttackType` / `MobTraits` / `MovementModeTag` / optional `MovementPoints`; `Posture::from_default_position` handles default-posture derivation. Wired consumers: combat `NaturalAttackType` → swing verb, wander AQUATIC gate, examine flavor lines, `mstat` readout. Stubbed consumers (bash / detect-undead / dispel-illusion / etc.) tracked in parking lot. |
| 2.M | Mob improvement (`riderPresenceMessage`, `aggressionFormula`, `activityRestrictions`) | Lua-driven mob behavior. |
| 2.N | `EntityVariables` | ✅ 2026-05-12 — `mud_db::entity_variables` loader hydrates `EntityVariableCache` at boot; Lua `self:setvar/getvar/clearvar` (and the same on `LuaRoom`) reads/writes the cache; mud-server flushes dirty bags every 10s via `upsert_many` / `delete`. |
| 2.O | `HelpEntry` | ✅ DB-backed `help` command — `mud_db::help::list_all`, `HelpCatalog` resource hydrated at boot, `cmd_help` falls through to keyword/title-prefix lookup after command + social matches; min_level gating mirrors `SystemText`. |
| 2.P | `Liquid` catalog | ✅ 2026-05-12 — `mud_db::liquids::list_all` hydrates `LiquidCatalog` (alias / color_desc / hunger/thirst/drunk deltas / description); `drink_amount` applies per-swig deltas from catalog, `cmd_taste`/`cmd_examine` render color when unidentified / name+description when identified, `cmd_pour`/`cmd_fill` canonicalize alias on transfer. Drunkenness decay already lives in `regen::drunkenness_tick`. |
| 2.Q | `Events` | Quest event triggers. Wire alongside quest trigger types. |
| 2.R | `Achievement.unlocked_at` | Read back for `achievements` listing. |
| 2.S | `CombatMessage` | Builder-authored hit/miss flavor variety. |
| 2.T | `PositionMessage` | Sit/stand/rest/sleep transition messages. |
| 2.U | `SystemMessage` | Error message variants. |
| 2.V | `PositionData` | Authoritative position metadata (appliedEffects, entryRequirement). |
| 2.W | `RaceSpellSlotBonus` | Per-race bonus slots. |
| 2.X | Race factors (`expFactor`, `hpFactor`, `hitDamageFactor`, `damageDiceFactor`, `copperFactor`, max stats, height/weight ranges, `enterVerb`/`leaveVerb`, `resistances`) | ✅ 2026-05-12 — `mud_db::races::list_all` + `RaceCatalog` resource hydrate the full Race row at boot. Consumers wired: `award_kill_xp` (`exp_factor`), `check_level_up` HP gain (`hp_factor`), combat swing damage (`hit_damage_factor`), natural-attack dice (`damage_dice_factor`), `award_kill_coin` (`copper_factor`), `cmd_train` cap clamp + character creation roll (`max_*` columns), `cmd_move` source/destination broadcast (`enter_verb` / `leave_verb`), `cmd_examine` size + lifeforce + body metrics, `spawn_player` rolls `BodyMetrics` from gender-resolved height/weight bands and folds race + class resistance JSON into the player `Resistances` component. |
| 2.Y | `CharacterClass` cols (`description`, `hit_dice`, `primary_stat`, `hp_per_level`, `resistances`) | ✅ 2026-05-12 — `mud_db::classes::ClassRow` + `ClassDef` carry the parity columns; `check_level_up` adds `class.hp_per_level` to the level-up HP gain, `spawn_player` folds `class.resistances` into the player's `Resistances` map alongside race resistances. `hit_dice` + `primary_stat` + `description` round-trip into the catalog for future consumers. |
| 2.Z | `LevelDefinition.permissions` (Permission[]) | Level-grant permissions. Parity. |
| 2.AA | Shop messages + `ShopFlag[]` + `ShopTradesWith[]` | Builder-authored shop dialog + behavior. Parity. |
| 2.AB | Shop item spawn (`spawn_chance`, `visibility_requirement`, `purchase_requirement` on ShopItems/ShopMobs) | Improvement. |
| 2.AC | `CharacterItems` instance state (`condition`, `custom_name`, `custom_examine_description`, `custom_values`, `instance_flags`, `liquid_effects`, `liquid_identified`) | Improvement (item degradation, player customization). |
| 2.AD | `Ability` metadata (`pages`, `memorization_time`, `quest_only`, `humanoid_only`, `is_toggle`, `contested_visibility`, `visibility_check`, `notes`, `tags`, `lua_script`, `school_id`) | Wire all (parity for spellbook; improvement for Lua scripts). |
| 2.AE | `AbilityRestrictions.custom_requirement_lua` | Dynamic ability gating. |
| 2.AF | `CharacterAbilities.last_used` | Cooldown integration. |
| 2.AG | `Trigger` validation metadata (`needs_review`, `syntax_error`) | Read at trigger load time + log. |

## Wave 3 — Combat redesign (sequential, complex)

Per user's "do it all now":

| # | Step | Status | Notes |
|---|---|---|---|
| 3.1 | Add Characters combat cols (accuracy/attack_power/spell_power/penetration_*/evasion/armor_rating/damage_reduction_percent/soak/hardness/ward_percent/perception/concealment/resistances) | ✅ done 2026-05-12 | Schema additions; defaults 0; pushed to fierydev |
| 3.2 | Rename `Mobs.armorRating`/`soak`/`wardPercent` semantics | ⚠️ partial | Schema kept original names; runtime conflates: `armor_rating + damage_reduction_percent → armor_pct` (clamped 100), `soak → armor_flat`. Naming rename deferred. |
| 3.3 | Drop `Mobs.damageReductionPercent` | ⏭️ deferred | Kept for builder clarity; folded at conversion time |
| 3.4 | Migrate `ObjectAffects` rows → `ObjectEffects` (modify-type) | ✅ done 2026-05-12 | `fierylib/scripts/migrate_object_affects.py` — 4083 rows → 3743 ObjectEffects rows (with `modifier_data={target,amount}`), 339 skipped (saving_*, cosmetic, MANA), 0 errored. Required dropping `(object_zone_id, object_id, effect_id)` unique constraint to allow multi-modify items. |
| 3.5 | Drop `ObjectAffects` table | ✅ done 2026-05-12 | Schema model + back-relation removed; `prisma db push --accept-data-loss` confirmed. |
| 3.6 | Delete `mud-db/src/object_affects.rs` module | ✅ done 2026-05-12 | File removed; `pub mod object_affects;` dropped from `lib.rs`; loader pass deleted from `mud-world/src/loader.rs`; `ObjectProto.affects` field removed. |
| 3.7 | `mud_server::equip_apply` reads new ObjectEffects modify rows | ✅ done 2026-05-12 | Rewrote `apply_object_to_wearer` to branch on `EffectDef.effect_type == "modify"` and route through `apply_modify_delta(target, amount)` from `modifier_data`. Added `_bonus` / `stamina_max` / `pen_flat` / `pen_pct` aliases to `apply_modify_delta`. Removed old `proto.affects` path and `map_legacy_apply_location`. |
| 3.8 | Drop `Mobs.hit_roll`, `armor_class` | ✅ done 2026-05-12 | Pushed |
| 3.8b | Drop `Mobs.hp_dice_*`, `damage_dice_*` | ⏭️ deferred | Kept — `MobProto::rolled_hp()`/`avg_damage()` use them for HP/base-damage roll. Orthogonal to accuracy/armor axis. |
| 3.9 | Drop `Characters.hit_roll`, `damage_roll`, `armor_class` | ✅ done 2026-05-12 | Pushed |
| 3.10 | Update `mud-db/src/mobs.rs` SELECT | ✅ done 2026-05-12 | Now selects 13 new combat cols |
| 3.11 | Update `mud-db/src/characters.rs` SELECT | ✅ done 2026-05-12 | Now selects 14 new combat cols |
| 3.12 | Update combat code paths to use new fields | ✅ done 2026-05-12 | combat.rs has proper d100 contest, NaturalDamage component, MAX_DAMAGE cap, roll_dice helper. login.rs reads new fields directly. resources.rs `MobProto::derived_combat_stats()` folds armor cols. |
| 3.13 | Tests: combat parity / balance regression suite | ⏭️ pending | See [parking-lot.md](./parking-lot.md) — hit_chance formula may not match docs/design/combat.md spec exactly |

**Build state (2026-05-12):** `cargo check` clean, `cargo test` 236 pass / 0 fail / 9 ignored.

## Wave 4 — Quest depth (wire-everything)

| # | Wire | Notes |
|---|---|---|
| 4.1 | `QuestTriggerType` dispatch — LEVEL/ITEM/ROOM/SKILL/EVENT/AUTO/MANUAL paths | ✅ 2026-05-12 — LEVEL (`combat::check_level_up`), ITEM (`bump_collect_quest_progress` path), ROOM (`mark_room_visited` path), SKILL (`bump_use_skill_quest_progress` path), AUTO (login spawn). MANUAL is admin tooling only (`qgive`/`qload`). EVENT dispatcher now wired — `mud_db::events::list_all` + `mud_server::events::EventsCatalog` poll every 60s; off → on edge fires `dispatch_event_trigger(world, event_id)`. |
| 4.2 | Quest trigger FK cols (`triggerMobZoneId`/etc., `triggerLevel`, `triggerItemZoneId`/etc., `triggerRoomZoneId`/etc., `triggerAbilityId`, `triggerEventId`) | ✅ 2026-05-12 — every trigger column read by `mud_db::quests::list_by_trigger_{level,item,room,ability,event}` / `list_auto_trigger`. |
| 4.3 | `Quests.timeLimitMinutes`, `cooldownMinutes` | ✅ 2026-05-12 — `accept_for_player` stamps `expires_at` from `time_limit_minutes` and gates cooldown via `AcceptOutcome::Cooldown { remaining_secs }`. |
| 4.4 | `Quests.exclusiveGroup` | ✅ 2026-05-12 — `accept_for_player` refuses with `AcceptOutcome::ExclusiveGroupConflict` when another non-ABANDONED quest in the same group is held. |
| 4.5 | `Quests.availabilityRequirement` (Lua) | ✅ 2026-05-12 — `eval_quest_availability` in `cmd_qaccept` evaluates the expression with the player as actor; non-truthy refuses with `AcceptOutcome::RequirementNotMet`. Fail-open on Lua errors. |
| 4.6 | `QuestObjective.luaExpression` (CUSTOM_LUA type) | ✅ 2026-05-12 — `quest_custom_lua_tick` sweeps every minute; `quest_custom_lua_drain` evaluates each pending row on the world thread, bumps progress when truthy, and `try_advance_phase` flows automatically. |
| 4.7 | `QuestObjective.internalNote` | ✅ 2026-05-12 — surfaced in `cmd_quests` objective listing for Builder+ viewers as a one-line "(builder note: …)" beneath the objective. |
| 4.8 | `QuestPhase.description` | ✅ 2026-05-12 — rendered beneath each phase header in `cmd_quests` when non-empty. |
| 4.9 | `QuestReward.choice_group` + `qreward` selection command | ✅ 2026-05-12 — `cmd_qreward [<zone> <id> [<reward-id>]]` lists every unclaimed group / claims a specific reward. Persistence is JSON: claim writes the reward id into `CharacterQuest.variables.claimed_rewards`. Grant goes through the same `grant_simple_rewards` + `PendingPlayerUpdate` path as auto-rewards. |
| 4.10 | `QuestReward.condition` (Lua) | ✅ 2026-05-12 — auto-grant path partitions on `condition` and skips conditional rewards (can't eval Lua from a tokio task). The synchronous `qreward` claim path evaluates the condition via `eval_quest_availability` before granting. UX polish (empty-state messaging / condition echo / pending-tally) wired 2026-05-12. |
| 4.11 | `CharacterQuests.variables` (JSON) | ✅ 2026-05-12 — column on `CharacterQuestRow`; `get_quest_variables` / `set_quest_variable` for round-trip. Consumed by `qreward` (`claimed_rewards` array) and surfaced to CUSTOM_LUA bodies as the `quest_vars_json` Lua extra. Trigger-body `quest:setvar`/`quest:getvar` Lua bindings wired 2026-05-12 via `LuaQuest` userdata + `QuestVariableCache` flush tick. |
| 4.12 | `CharacterQuests.expires_at` | ✅ 2026-05-12 — `accept_for_player` stamps it; `quest_sweep_tick` (every 60 simulated seconds) calls `fail_expired_quests` which flips rows to FAILED and notifies online holders. |
| 4.13 | `QuestDialogue` + `DialogueTrees` + `DialogueNodes` + `DialogueResponses` | ✅ 2026-05-12 — `quest_dialogue::load_catalog` hydrates the in-memory `DialogueCatalog` at boot from all four tables. `cmd_ask` calls `try_advance_active_tree` (mid-tree fast path, no DB) then falls back to `dispatch_dialogue_attempt` (per-objective lookup, async). REGEX matcher fully wired 2026-05-12 via the `regex` crate. |

**Build state (2026-05-12):** `cargo check` clean (4 pre-existing combat/login warnings, unrelated to Wave 4); `cargo test` 316 pass / 0 fail / 18 ignored.

## Wave 5 — Account features

| # | Wire | Notes |
|---|---|---|
| 5.1 | `Users.account_wealth` | ✅ 2026-05-12 — `AccountWealth` component hydrated from `Users.account_wealth` at `spawn_player`; `account_balance` / `account_deposit` / `account_withdraw` commands (aliases `abal` / `adeposit` / `awithdraw`) transfer between per-character `bank_wealth` and the shared pool. Cross-character sync via `account_bank::fanout_account_wealth` walks every online sibling on the same `user_id`. `users::save_account_wealth` writes back in `save_player`'s transaction. |
| 5.2 | `AccountItems` | ✅ 2026-05-12 — `chest` / `chest_deposit` / `chest_withdraw` (async commands; aliases `achest` / `accountchest`). Per-instance state (`charges`, `liquid_remaining`, `liquid_type`, `light_remaining`) round-trips via `custom_data` JSON. SOULBOUND / NO_DROP gates honored. DB layer: `mud_db::account_items::{list_for_user, deposit, withdraw}`. |

## Wave 6 — Federated identity

| # | Wire | Notes |
|---|---|---|
| 6.1 | `DiscordLink` | ✅ 2026-05-12 — `mud_db::discord_links::{for_user, for_discord_id, link, unlink, mark_verified}` covers the bot ingress + in-game `discord link <id>` / `discord unlink` (Settings category). Verification flow is two-step: in-game `discord link` mints a 6-digit code into the `PendingDiscordLinks` runtime resource (10-min TTL, aged off by a 30s tick — formerly shared with the `login_requests` sweep, which has been removed); the bot (out-of-process, Muditor-side) consumes the code from gossip and writes the row. Linked Discord renders on the unified `account` command. Live-DB round-trip test: `discord_link_round_trip` (mud-db integration). |
| 6.2 | `GoogleLink` | ✅ 2026-05-12 — `mud_db::google_links::{for_user, link, unlink}` exposes the OAuth callback path (Muditor writes) + in-game readout. Linked Google renders on the unified `account` command. Live-DB round-trip test: `google_link_round_trip`. |
| 6.3 | `DiscordConfig` | ✅ 2026-05-12 — `mud_db::discord_config::get` reads the singleton row at PK 1; loader publishes `DiscordConfigCatalog` resource at boot. `can_send_gossip` / `can_send_admin` / `can_send_announcement` gate any in-process broadcast that wants to decorate outbound traffic with a destination tag. The bot itself runs out-of-process (Muditor-side) — see the parking lot. Live-DB shape test: `discord_config_get_shape`. |
| 6.4 | Character name approval | ✅ 2026-05-12 — replaces the original `LoginRequests` row-based approval flow (table dropped, `LoginRequestStatus` enum gone, `LoginStage.LOGIN_APPROVAL_*` variants removed). New design: per-character `Characters.name_approved Boolean @default(true)` plus a `NameApprovalPending` runtime marker. When the live `social.name_approval_required` GameConfig flag is ON, fresh characters insert at `name_approved = false`; on spawn the runtime attaches `NameApprovalPending`. The player can still move / look / fight, but every social command (`tell` / `reply` / `say` / `whisper` / `gsay` / `gossip` / `music` / `shout` / `qsay` / `ctell` / `invite`) refuses with a "your name is awaiting staff approval" line until the marker is removed. Staff resolves via Immortal+ `approve_name <character>` (keep the name → flip column + drop marker live + DM the player) or `reject_name <character> <new>` (force-rename + auto-approve; refuses while target is online so the Named cache rebuilds via reconnect). Players run `name_status` to self-diagnose. Existing characters are grandfathered (column default `true` so the schema migration was zero-effort). Live-DB tests: `character_name_approval_round_trip`, `character_name_approval_defaults_true`. Unit tests: `name_approval_gate_blocks_when_marker_present`, `name_approval_gate_clears_when_marker_removed`. |

## Wave 7 — Followup (post-everything)

**Build state (2026-05-12):** `cargo check` clean (4 pre-existing dead-code warnings), `cargo test` 316 pass / 0 fail / 18 ignored, live-DB suite 18 pass / 0 fail. World boots clean (134 zones / 10296 rooms / 1807 spawned mobs / 472 commands). The only remaining `equip_apply` warnings on boot are intentional skips for cosmetic `AGE` / `CHAR_HEIGHT` / `CHAR_WEIGHT` / `COMPOSITION` rows (per the ObjectAffects mapping parking-lot entry).

| # | Step | Notes |
|---|---|---|
| 7.1 | Sweep codebase for TODOs / `// deferred` / `// follow-up` comments; fix or track | ✅ 2026-05-12 — only 2 hard `TODO` markers remain, both pre-existing and tracked in parking lot. Stale docstrings (hunger/thirst/`Title` setter/TimePlayed) updated inline to reflect wired state. `run_room_trigger` and `wait_until` Lua stubs newly entered in parking lot. |
| 7.2 | Run full test suite; address regressions | ✅ 2026-05-12 — 316 pass / 0 fail / 18 ignored unit + integration; 18 pass / 0 fail live-DB ignored suite. No regressions vs. last green. |
| 7.3 | Update `database-audit.md` final status (all items ✅ or moved to parking lot) | ✅ 2026-05-12 — top-of-file migration progress block rewritten; stale per-field rows for Mob combat cols / Mob hit_roll/armor_class / Object flags/restrictions / Room boolean flags / Character hit_roll/damroll/ac / height/weight updated. |
| 7.4 | Compaction pass: prune stale doc references to dropped tables | (no stale refs found — Wave 1 drops cleanly removed from audit and migration plan) |
| 7.5 | Inline fix: `equip_apply` mapping for modern combat column names (`ATTACK_POWER`/`ACCURACY`/`SPELL_POWER`/`EVASION`/`ARMOR_RATING`/`HARDNESS`/`WARD_PCT`) | ✅ 2026-05-12 — `map_legacy_apply_location` now accepts both legacy CircleMUD names (with scale) and modern combat-redesign names (pass-through). Eliminates ~thousands of boot warnings from imported `ObjectAffects` rows. |
