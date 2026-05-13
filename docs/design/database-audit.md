# Database Audit

**Generated:** 2026-05-12
**Scope:** Every model in `muditor/packages/db/prisma/schema.prisma` cross-referenced against the `mud-db` Rust loaders and downstream consumption in `mud-server` / `mud-world` / `mud-script`. Muditor-only tables are listed for completeness but not audited.
**Companion doc:** [`schema-reconciliation.md`](./schema-reconciliation.md) — narrower clean-up plan for already-decided drops. This document is broader: it enumerates every field, not just the contested ones.

## Status legend

| Marker | Meaning |
|---|---|
| ✅ | Loaded by `mud-db` **and** consumed downstream |
| ⚠️ | Loaded but only partially consumed (display-only, admin-tools-only, or routed but not yet active in pipeline) |
| 🔶 | Loaded but never read after load (dead read) |
| ❌ | Not loaded by `mud-db` at all |
| 🗑️ | Recommend **drop** — obsolete column / duplicate / dead design |
| 🔌 | Recommend **wire** — schema exists, runtime should consume it |
| 🏗️ | Muditor-only — schema exists for the web editor / FieryLib import path; runtime shouldn't read |

---

## Migration progress

**As of 2026-05-12:** all waves complete (Wave 7 is the post-everything sweep).
- ✅ Wave 1 (safe drops): 14 tables, 27 columns, 5 enums dropped.
- ✅ Wave 2 (latent wires): Object flags + restrictions, Room boolean flags, Mob latent (size/lifeForce/damageType/move/defaultPosition/traits/movementMode/defaultMovementMode), EntityVariables, HelpEntry, Liquid catalog, Race factors, Class factors, etc. Pending consumers tracked in [`parking-lot.md`](./parking-lot.md).
- ✅ Wave 3 (combat redesign): Characters combat cols added; legacy `hit_roll`/`damage_roll`/`armor_class`/`hp_dice_*`/`damage_dice_*` (Characters) dropped; `hit_roll`/`armor_class` on Mobs dropped; mud-db loaders updated; combat math uses new d100 contest. `ObjectAffects` → `ObjectEffects` (modify-type) migration completed 2026-05-12 (3743 rows backfilled, table dropped).
- ✅ Wave 4 (quest depth): all trigger types, time/cooldown/exclusive-group/availability gates, choice + conditional rewards (`qreward`), CharacterQuests.variables JSON, dialogue trees (REGEX matcher parked).
- ✅ Wave 5 (account features): `Users.account_wealth` (shared bank, `chest_*` commands), `AccountItems` (shared chest).
- ✅ Wave 6 (federated identity): `DiscordLink`/`GoogleLink`/`DiscordConfig` + per-character name approval (replaces the dropped `LoginRequests` design; `Characters.name_approved` column + `NameApprovalPending` marker + Immortal+ `approve_name` / `reject_name` / `name_status`). Out-of-process Discord bot still parked.

See [`migration-plan.md`](./migration-plan.md) for the per-step tracker and [`parking-lot.md`](./parking-lot.md) for items needing user feedback later.

## Action items (top-priority)

These are concrete migrations the project should schedule. Order is the recommended sequence; each item is a discrete migration.

### Drop (obsolete / duplicate / dead)

1. **`Triggers.mobZoneId`, `mobObjectId`, `objectZoneId`, `objectId`** — direct FK columns superseded by `MobTriggers` / `ObjectTriggers` / `RoomTriggers`. Runtime reads only the junction tables. *(Already in [`schema-reconciliation.md`](./schema-reconciliation.md).)*
2. **`Triggers.variables`** — JSON column intended for "Lua trigger persistent vars," but the runtime now uses `EntityVariables` (wired 2026-05-12). Drop the column. *(In reconciliation doc.)*
3. **`Characters.raceType`** — text shadow of the `race` enum; runtime reads the enum. Drop. *(In reconciliation doc.)*
4. **`Characters.playerClass`** — text shadow of `classId`; runtime reads the FK. Drop. *(In reconciliation doc.)*
5. **`Characters.baseHeight`, `baseWeight`, `baseSize`, `currentSize`** — six body-metric columns; runtime reads `height` / `weight` only. Drop the four `base*` / `current*` columns. *(In reconciliation doc.)*
6. **`Characters.averageStats`** — never loaded by `mud-db`. Drop.
7. **`Mobs.averageStats`** — never loaded. Drop.
8. **`Mobs.composition`, `Mobs.stance`, `Mobs.estimatedHp`, `Mobs.raceAlign`** — still unloaded; no consumer planned. Drop.
   **Wired (2026-05-12 — Wave 2.L):** `Mobs.size`, `Mobs.lifeForce`, `Mobs.damageType`, `Mobs.move`, `Mobs.defaultPosition`, `Mobs.traits`, `Mobs.movementMode`, `Mobs.defaultMovementMode` — loaded by `mud-db::mobs::list_mobs`, spawned onto each mob instance as `Sized` / `LifeForceTag` / `NaturalAttackType` / optional `MovementPoints` / `Posture` / `MobTraits` / `MovementModeTag`. Consumers live in `combat.rs` (swing verb), `wander.rs` (AQUATIC sector gate), `info::cmd_examine` (flavor lines), and `admin_inspect::cmd_mstat` (proto readout).
   **Pending (Wave 2.M):** `Mobs.riderPresenceMessage`, `Mobs.aggressionFormula`, `Mobs.activityRestrictions` — Lua-formula fields, deferred.
9. **`Mobs.strength`/`intelligence`/`wisdom`/`dexterity`/`constitution`/`charisma`** — stat columns not yet loaded by `mud-db::mobs`. Wire when mob-stat system needs them.
   **Wired:** the rest of the combat-redesign columns — `armorRating`, `damageReductionPercent`, `soak`, `hardness`, `perception`, `concealment`, `attackPower`, `spellPower`, `penetrationFlat`, `penetrationPercent`, `evasion`, `accuracy`, `resistances` — are loaded today. `traits` ✅ Wave 2.L (2026-05-12).
10. **`Ability.classes`** — JSON shorthand duplicated by `ClassAbilities`. Drop. *(In reconciliation doc.)*
11. **`MobCarrying`** entire table — overlaps `MobResetEquipment`; not loaded. Drop. *(In reconciliation doc.)*
12. **`EquipmentSets` + `MobEquipmentSets` + `EquipmentSetItems`** entire tables — never loaded by `mud-db`. Drop. *(In reconciliation doc.)*
13. ~~**`ObjectAffects`** entire table~~ — 🗑️ dropped 2026-05-12 (Wave 3.4-3.7). Rows backfilled into `ObjectEffects` (modify-type) with `modifier_data = {target, amount}`; runtime `equip_apply` branches on `effect_type == "modify"` and routes through `apply_modify_delta`. See `fierylib/scripts/migrate_object_affects.py`.
14. **`Sector` planar variants** (`ASTRALPLANE`, `AIRPLANE`, `FIREPLANE`, `EARTHPLANE`, `ETHEREALPLANE`, `AVERNUS`) — no runtime branch differentiates these from `OUTDOOR`/`CAVE`. Wire to `RoomEnvironmentalEffect` (preferred) or fold into a single `PLANAR` value. *(In reconciliation doc.)*
15. **`Composition` enum + `Races.defaultComposition`** — neither column loaded, no consumer. Drop together. *(In reconciliation doc.)*
16. **`MagicAffinity` enum + `Room.magicAffinity`** — declared, never read. Drop. *(In reconciliation doc.)*
17. **`Stance` enum + `Mobs.stance`** — declared, never read. Posture is on `Position` now. Drop the enum.
18. **`ShopHours`, `ShopRooms`, `ShopAbilities`** — none loaded. Drop. *(In reconciliation doc.)*
19. **`PlayerToggle`** entire table — `PlayerFlag` enum + `PlayerFlags` component cover all toggles. Drop. *(In reconciliation doc.)*
20. **`ShapechangeForm` + `AbilityShapechangeForm`** — feature not implemented; not loaded. Drop. *(In reconciliation doc.)*
21. **`DeploymentPackage` + `DeploymentChange`** — Muditor-only operational tooling, not a runtime concern. Drop or move out of the shared schema. *(In reconciliation doc.)*
22. **`PlayerMail` 18 columns** (`legacySenderId`, `legacyRecipientId`, all `attachedCopper/Silver/Gold/Platinum`, `attachedObject*`, `wealthRetrieved*`, `objectRetrieved*`, `objectMovedToAccountStorage`, etc.) — `mud-db` uses only `AccountMail`; `PlayerMail` is unread. Either drop the whole table or drop the legacy/attachment columns. The 2026-04 mail rewrite chose `AccountMail` exclusively.
23. **`SpawnConditions`** entire table — never loaded. Spawn behavior is `MobResets.resetBehavior` + `probability` today. Drop.
24. **`DiscordLink`, `GoogleLink`, `DiscordConfig`** — ✅ wired Wave 6 (2026-05-12). See `### DiscordLink / GoogleLink / DiscordConfig` for the per-table breakdown.
25. **`PositionData`, `PositionMessage`, `SystemMessage`** — none loaded. Decide: wire as builder-authored flavor text (parallel to `SystemText`) or drop.
26. **`CombatMessage`** — declared in schema, not loaded. Decide: wire for hit/miss flavor variety or drop.
27. **`PositionMechanic` enum + `Room.requiredMechanic`** — never read. Drop.
28. **`Room.entryRestriction`, `allowsMagic`, `allowsRecall`, `allowsSummon`, `allowsTeleport`, `isDeathTrap`, `isIndoors`, `isSoundproof`, `isArena`, `isGuildhall`, `allowsMobs`, `allowsTracking`, `allowsPortals`, `allowsScanning`** — 14 flags not loaded by `mud-db::rooms`. Decide per-flag: wire what the runtime actually needs (room-peaceful is already wired; the rest follow the same shape) or drop.
29. **`Quests.timeLimitMinutes`, `cooldownMinutes`, `exclusiveGroup`, `availabilityRequirement`, all `trigger*` columns** — ✅ wired Wave 4 (2026-05-12). See the "Quests / QuestPhases / QuestObjectives / QuestPrerequisites / QuestRewards / CharacterQuests / CharacterQuestObjectives" section for the per-column breakdown.
30. **`Effect.delaySeconds`, `tickIntervalSec`** — schema fields not loaded by `mud-db::effects`. Wire if effect timing should be data-driven, else drop.
31. **`Mobs.role`** — was previously thought unused. **Correction:** the field *is* consumed by `combat::xp_multiplier_for_role` (Trash=50, Boss=1000, etc.). Keep.
32. **Characters `hit_roll`, `damage_roll`, `armor_class`** — **Dropped 2026-05-12** (Wave 3.9). New combat model uses `accuracy`/`attack_power`/`armor_rating`/etc. on Characters too; loader and admin tooling already read the new fields.
33. **`SaveType` enum** — schema declares modern (`REFLEX`/`FORTITUDE`/`WILL`) but `ApplyType` still carries legacy `SAVING_PARA`/`SAVING_ROD`/`SAVING_PETRI`/`SAVING_BREATH`/`SAVING_SPELL`. Pick one set; remove the other. The runtime reads neither today.
34. **`ItemInstanceFlag` enum** — `CharacterItems.instanceFlags` column exists; `mud-db::character_items` doesn't select it. Drop the column unless the runtime grows item-instance state (cursed/attuned/identified gating).
35. **`CharacterItems.custom_name`, `custom_examine_description`, `custom_values`, `condition`, `liquid_effects`, `liquid_identified`** — none loaded. Drop or wire (player-customizable items, item degradation).

### Wire (latent content already authored / planned)

1. **`EntityVariables`** — ✅ wired 2026-05-12. `mud-db/src/entity_variables.rs` provides `list_all` / `list_for_entity` / `upsert` / `upsert_many` / `delete`; `mud-world::EntityVariableCache` resource hydrated once at boot; Lua `self:setvar/getvar/clearvar` (and the same on `LuaRoom`) reads/writes the cache; `mud-server::entity_vars::entity_var_flush_tick` flushes dirty bags every 10s.
2. **`SystemText` + `LoginMessage` + `GameConfig`** — **already wired** as of recent loader passes. Verified consumption in `mud-server/src/commands/info.rs::system_text_with_fallback`, `mud-server/src/login.rs::login_message_bytes`, and `mud-server/src/main.rs` (`RuntimeConfig` resource). Keep, watch for new keys/categories.
3. **`HelpEntry`** — ✅ wired. `mud-db/src/help.rs` + `HelpCatalog` resource + `cmd_help` fallthrough. Min-level gated; case-insensitive keyword exact-match with title-prefix fallback.
4. **`Quests` / `QuestObjectives` / `QuestPrerequisites` / `CharacterQuest` / `CharacterQuestObjective` / `QuestPhase` / `QuestReward`** — ✅ fully wired (KILL_MOB / COLLECT_ITEM / TALK_TO_NPC / VISIT_ROOM / USE_SKILL / DELIVER_ITEM / CUSTOM_LUA). Wave 4 (2026-05-12) closed the remaining gaps: choice-group rewards via `qreward`, `QuestDialogue` + `DialogueTrees` + `DialogueNodes` + `DialogueResponses` loaded into the runtime `DialogueCatalog`.
5. **`ConsumableEffects`** — loaded and consumed by `cmd_consume` / `cmd_quaff`. Keep.
6. **`Liquid`** — ✅ wired 2026-05-12. `mud-db/src/liquids.rs::list_all` loads the table; `mud-world::LiquidCatalog` resource hydrated at boot with per-row payload (alias / color_desc / hunger/thirst/drunk deltas / description). Consumed by `drink_amount` (hunger/thirst/drunk applied per swig from catalog), `cmd_taste` (real name from catalog), `cmd_pour`/`cmd_fill` (alias canonicalization on transfer), and `cmd_examine` (color_desc when unidentified, name+description when identified).
7. **`Events`** — not loaded. Wire for seasonal content / `qtype = EVENT` quest triggers.

### Promote text columns to typed enums

1. **`MobResetEquipment.wearLocation` (String)** → `WearLocation` enum. Same enum should replace `CharacterItems.equippedLocation`. *(In reconciliation doc.)*
2. **`AbilitySavingThrow.onSaveAction` (JSON-of-string)** → `SaveResult` enum. *(In reconciliation doc.)*
3. **`MobResets.resetBehavior` (String, default `"PERSISTENT"`)** → `ResetBehavior` enum.
4. **`ObjectResets.resetBehavior`** → same enum.
5. **`Mobs.race` / `Characters.race`** — already enum; runtime reads as raw text via `::text` casts. Either keep the cast (today's pattern) or generate a Rust enum mirror; not a schema action.

---

## Audited models — field-by-field

Each section: model summary, then a per-field table. Loaded-by-rs uses **mud-db file:struct** as the citation.

### Mobs (`Mobs`)

Schema at `schema.prisma:783-903`. Composite key `(zoneId, id)`. Rust loader: `mud-db/src/mobs.rs::Mob` (24 columns).

| Field | Status | Notes / consumer |
|---|---|---|
| `zone_id`, `id` | ✅ | Composite key. |
| `keywords`, `name`, `room_description`, `examine_description`, `level`, `alignment` | ✅ | Core proto data. |
| `role` (`MobRole`) | ✅ | `combat::xp_multiplier_for_role`. |
| `hp_dice_*`, `damage_dice_*` | ✅ | Consumed by `MobProto::rolled_hp()` / `avg_damage()` for spawn-time HP and base damage. Kept deliberately — orthogonal to the accuracy/armor axis. Parked review at [`parking-lot.md`](./parking-lot.md). |
| `hit_roll`, `armor_class` | — | **Dropped 2026-05-12** (Wave 3.8). Replaced by `accuracy` + `armor_rating` + `damage_reduction_percent`. |
| `ward_percent` | ✅ | `combat::apply_damage` (magical mitigation). |
| `wealth` | ✅ | Paid to killer. |
| `class_id` | ✅ | Trigger gating via `actor.class`. |
| `behaviors` (`MobBehavior[]`) | ✅ | `respawn::spawn_mob`, `admin_inspect`. |
| `protected_kind` | ✅ | Alignment penalty on player kill. |
| `professions` (`MobProfession[]`) | ✅ | Banker/shopkeeper/trainer gating. |
| `gender`, `race` | ✅ | Display + Lua `actor.gender`/`actor.race`. |
| `accuracy`, `evasion`, `attack_power`, `spell_power`, `penetration_flat`, `penetration_percent`, `armor_rating`, `damage_reduction_percent`, `soak`, `hardness`, `perception`, `concealment` | ✅ | Combat redesign columns — loaded (Wave 3.10), folded by `MobProto::derived_combat_stats()` into `armor_pct`/`armor_flat`/etc., consumed by `combat.rs` d100 contest. |
| `resistances` (JSON) | ✅ | Loaded, folded into `Resistances` map on each spawn, consumed by element-typed damage application. |
| `strength`, `intelligence`, `wisdom`, `dexterity`, `constitution`, `charisma` | ❌ | Not loaded. Wire when stat system on mobs is needed (saving-throw bonuses, social skill checks). |
| `size`, `life_force`, `damage_type`, `move`, `default_position`, `traits`, `movement_mode`, `default_movement_mode` | ✅ | Loaded by `mud-db::mobs::list_mobs` and surfaced as `Sized` / `LifeForceTag` / `NaturalAttackType` / `MovementPoints` / `Posture` / `MobTraits` / `MovementModeTag` components at every spawn site (loader, respawn, summon). Examine + `mstat` render them; `combat.rs` consumes `NaturalAttackType` for the swing verb; `wander.rs` consumes `MobTraits::Aquatic`. Parking-lot entries cover the remaining consumer stubs (bash / detect-undead / etc.). |
| `estimated_hp`, `race_align`, `composition`, `stance`, `position` | 🗑️ | None loaded; no consumer. **Action items #8, #17.** |
| `rider_presence_message`, `aggression_formula`, `activity_restrictions` | 🗑️ | None loaded; either wire as Lua-formula fields or drop. **Action item #8.** |
| `average_stats` | 🗑️ | Never loaded. **Action item #7.** |
| `created_at`, `updated_at`, `deleted_at`, `created_by`, `updated_by` | 🏗️ | Muditor audit columns. |

### Objects (`Objects`)

Schema at `schema.prisma:973-1062`. Composite key `(zoneId, id)`. Rust loader: `mud-db/src/objects.rs::Object` (15 columns).

| Field | Status | Notes |
|---|---|---|
| `zone_id`, `id`, `type`, `keywords`, `name`, `room_description`, `examine_description`, `level`, `weight`, `cost`, `values` (JSON) | ✅ | Core proto. |
| `wear_flags` | ✅ | Equip slot validation. |
| `restricted_alignments`, `restricted_class_ids`, `restricted_races` | ✅ | Equip-time gating + display. |
| `plain_name`, `base_name`, `plain_base_name`, `plain_room_description`, `plain_examine_description`, `action_desc`, `plain_action_desc`, `article` | ❌ | Muditor-side normalization for search. Not consumed by runtime. **Action:** keep (cheap, harmless) or drop the `plain_*` mirrors. |
| `timer`, `decompose_timer` | ❌ | Decay/timer system not yet runtime. Wire or drop. |
| `concealment` | ❌ | Not loaded. Drop if no plan to wire stealth-on-items. |
| `flags` (`ObjectFlag[]`), `restrictions` (`ObjectRestriction[]`) | ✅ | Loaded by `objects.rs`; stamped on every spawn site as `ObjectFlags(Vec<…>)` / `ObjectRestrictions(Vec<…>)`. Consumer gates: `cmd_look`/`cmd_examine` (INVISIBLE/HUM), `save_player` (TEMPORARY filter), `cmd_drop` (NO_DROP), `cmd_sell` (NO_SELL), etc. Pending consumer flags tracked in [`parking-lot.md`](./parking-lot.md). |
| `allowed_races`, `min_size`, `max_size` | ❌ | Inclusive-restriction columns. Not loaded; not yet in equip-validation path. Wire when needed. |
| `passenger_capacity` | ❌ | `VEHICLE` items only. Not wired. |
| `presence_override` | ❌ | "Flying carpet" presence message. Wire if vehicles/mounts surface. |
| `fixture_room_zone_id`, `fixture_room_id` | ❌ | "Permanent fixture" — object always exists in a specific room. Not loaded. |
| `created_at`, `updated_at`, `deleted_at`, `created_by`, `updated_by` | 🏗️ | Muditor. |

### ~~ObjectAffects~~ 🗑️ dropped 2026-05-12

Wave 3.4-3.7 retired this table. Rows were backfilled into `ObjectEffects` (modify-type) via `fierylib/scripts/migrate_object_affects.py` and the table dropped. The legacy `mud-db/src/object_affects.rs` loader is gone; the runtime path now lives in `mud_server::equip_apply` and branches on `EffectDef.effect_type == "modify"` (calling `apply_modify_delta(target, amount)` from `modifier_data`).

### ObjectEffects

Schema at `schema.prisma:474-489`. Rust loader: `mud-db/src/object_effects.rs::ObjectEffectRow`.

| Field | Status |
|---|---|
| `object_zone_id`, `object_id`, `effect_id`, `strength`, `modifier_data`, `wear_location` | ✅ |
| `id` (PK), unique key | ✅ | (Indexed-only.) |

### ObjectResistance

Schema at `schema.prisma:924-935`. Rust loader: `mud-db/src/object_resistance.rs::ObjectResistanceRow`. All fields ✅.

### ObjectExtraDescriptions

Schema at `schema.prisma:914-921`. Rust loader: `mud-db/src/object_extra_descriptions.rs::ObjectExtra`.

| Field | Status |
|---|---|
| `object_zone_id`, `object_id`, `keywords`, `description` | ✅ |
| `id` (PK) | 🔶 | Not SELECTed but auto-PK. |

### ObjectAbilities

Schema at `schema.prisma:241-252`. Rust loader: `mud-db/src/object_abilities.rs::ObjectAbilityRow`. All fields ✅.

### ObjectResets

Schema at `schema.prisma:939-955`. Rust loader: `mud-db/src/object_resets.rs::ObjectReset`.

| Field | Status |
|---|---|
| `id`, `object_zone_id`, `object_id`, `room_zone_id`, `room_id`, `max_instances`, `probability`, `reset_behavior` | ✅ |
| `zone_id`, `comment` | 🔶 | Not loaded. |

### ObjectResetContents

Schema at `schema.prisma:958-971`. Rust loader: `mud-db/src/object_reset_contents.rs::ObjectResetContent`. All non-comment fields ✅.

### Rooms (`Room`)

Schema at `schema.prisma:1177-1251`. Composite key `(zoneId, id)`. Rust loader: `mud-db/src/rooms.rs::Room` (11 columns).

| Field | Status | Notes |
|---|---|---|
| `zone_id`, `id`, `name`, `room_description`, `sector` | ✅ | Core. |
| `base_light_level` | ✅ | Lighting render. |
| `capacity` | ✅ | Combat occupancy cap. |
| `is_peaceful` | ✅ | `PeacefulRoom` marker. |
| `layout_x`, `layout_y`, `layout_z` | ✅ | GMCP mapper frame. |
| `magic_affinity` | 🗑️ | Not loaded. **Action item #16.** |
| `required_mechanic` | 🗑️ | Not loaded. **Action item #27.** |
| `entry_restriction` (Lua) | 🔌 | Lua-based entry gate. Wire when the entry pipeline lands. |
| `allows_magic`, `allows_recall`, `allows_summon`, `allows_teleport`, `is_death_trap`, `is_indoors`, `is_soundproof`, `is_arena`, `is_guildhall`, `allows_mobs`, `allows_tracking`, `allows_portals`, `allows_scanning` | ✅ | All 13 loaded (Wave 2.I) and stamped as marker components (`NoMagicRoom` / `NoRecallRoom` / `DeathTrapRoom` / etc.). Consumer gates wired for most; ArenaRoom/GuildhallRoom/NoPortalsRoom track pending consumers in [`parking-lot.md`](./parking-lot.md). |
| `plain_room_description` | 🏗️ | Muditor-side search normalization. |
| `created_at`, `updated_at`, `deleted_at`, `created_by`, `updated_by` | 🏗️ | Muditor. Loader uses `deleted_at IS NULL` filter. |

### RoomExit

Schema at `schema.prisma:1142-1166`. Rust loader: `mud-db/src/room_exits.rs::RoomExit`.

| Field | Status | Notes |
|---|---|---|
| `id`, `room_zone_id`, `room_id`, `direction`, `to_zone_id`, `to_room_id`, `default_state`, `key_zone_id`, `key_id`, `description`, `keywords`, `flags` | ✅ | All loaded. |
| `hit_points` (bashable doors) | ❌ | Wire when bash mechanics land, or drop. |

### RoomEnvironmentalEffect

Schema at `schema.prisma:1254-1264`. Rust loader: `mud-db/src/room_environmental_effects.rs::RoomEnvironmentalEffectRow`. All fields ✅.

### RoomExtraDescriptions

Schema at `schema.prisma:1168-1175`. Rust loader: `mud-db/src/room_extra_descriptions.rs::RoomExtra`. All non-PK fields ✅.

### Zones (`Zones`)

Schema at `schema.prisma:1615-1636`. Rust loader: `mud-db/src/zones.rs::Zone` (9 columns).

| Field | Status |
|---|---|
| `id`, `name`, `lifespan`, `reset_mode`, `hemisphere`, `climate`, `created_at`, `updated_at`, `deleted_at` | ✅ |
| `created_by`, `updated_by` | 🏗️ | Muditor audit. |

### MobResets

Schema at `schema.prisma:763-781`. Rust loader: `mud-db/src/mob_resets.rs::MobReset`.

| Field | Status | Notes |
|---|---|---|
| `id`, `mob_zone_id`, `mob_id`, `room_zone_id`, `room_id`, `max_instances`, `probability`, `reset_behavior` | ✅ | |
| `zone_id`, `comment` | 🔶 | Loaded into the schema but not SELECTed by `mob_resets.rs`. |

### MobResetEquipment

Schema at `schema.prisma:747-761`. Rust loader: `mud-db/src/mob_reset_equipment.rs::MobResetEquipment`. All fields ✅. **Action item:** promote `wear_location` to enum (action item promote-1).

### MobAbilities

Schema at `schema.prisma:314-325`. **No `mud-db` module reads this table.** Runtime resolves mob abilities through `MobDefaultEffects` + ability catalog. Decide: wire (mob-specific spellbook?) or drop in favor of class-based ability list.

| Field | Status |
|---|---|
| All | ❌ — not loaded. |

### MobDefaultEffects

Schema at `schema.prisma:457-471`. **No dedicated `mud-db` module.** Loaded inline in `mud-world/src/loader.rs` (per `effects.md`). Fields used: `mob_zone_id`, `mob_id`, `effect_id`, `strength`, `modifier_data`.

### MobCarrying

Schema at `schema.prisma:722-734`. Not loaded. **Drop.** *Action item #11.*

### MobEquipmentSets / EquipmentSets / EquipmentSetItems

`schema.prisma:700-745`. Not loaded. **Drop.** *Action item #12.*

### CharacterPets

Schema at `schema.prisma:683-698`. Not loaded by `mud-db`. Pets round-trip through `Characters.pets` (JSON column) instead — the persisted-pet system uses a JSON blob for the on-disconnect-1-hour cap. Decide: migrate to the normalized `CharacterPets` table (more queryable) or drop the table.

### Characters (`Characters`)

Schema at `schema.prisma:533-671`. The biggest persistence table. Rust loader: `mud-db/src/characters.rs::CharacterRow` (44 columns) + ~12 dedicated JSON load/save functions for round-trip blobs.

| Field group | Status | Notes |
|---|---|---|
| `id`, `name`, `user_id`, `level`, `alignment`, `class_id`, `race`, `gender`, `experience`, `title`, `description`, `prompt` | ✅ | Core identity. |
| `hit_points`, `hit_points_max`, `stamina`, `stamina_max` | ✅ | Resource bars. |
| `strength`, `intelligence`, `wisdom`, `dexterity`, `constitution`, `charisma` | ✅ | Core stats. |
| `hit_roll`, `damage_roll`, `armor_class` | ⚠️ | Admin inspect + combat fallback. **Drop** with combat redesign. **Action item #32.** |
| `permissions`, `player_flags`, `position` | ✅ | Persisted state. |
| `current_room_*`, `recall_room_*` | ✅ | Position round-trip. |
| `wealth`, `bank_wealth` | ✅ | Currency. |
| `skill_points`, `hunger`, `thirst`, `time_played`, `last_login`, `invis_level`, `freeze_level`, `wimpy_threshold`, `poof_in`, `poof_out` | ✅ | Round-trip persistent state. |
| `drunkenness` | ✅ | Dedicated `load_drunkenness` / `save_drunkenness` (column-only). |
| `staff_notes` | ✅ | Dedicated `load_staff_notes` / `save_staff_notes`. |
| `kill_tracking_data` (JSON) | ✅ | XP diminishing returns. |
| `script_vars` (JSON) | ✅ | Lua per-player vars. |
| `trophy_data` (JSON) | ✅ | Trophy XP modifier. |
| `spell_cooldowns` (JSON) | ✅ | Circle-pool cooldowns. |
| `cooldowns` (JSON) | ✅ | Per-ability cooldowns. |
| `ignore_list` (JSON) | ✅ | Tells block-list. |
| `effect_instances` (JSON) | ✅ | Active effects, 1h disconnect cap. |
| `pets` (JSON) | ✅ | Hired/charmed pets snapshot. |
| `password_hash` | ⚠️ | Vestigial — runtime authenticates via `Users.password_hash`. Kept for legacy callers. |
| `is_online` | 🔶 | Not loaded. Could be used for a `who` count without iterating ECS but isn't. Decide: keep + maintain on connect/disconnect, or drop. |
| `birth_time` | 🔶 | Not loaded. Drop or surface in `score`. |
| `wiz_title` | 🔶 | Not loaded. Drop if `title` covers immortal use. |
| `auto_invis_level` | 🔶 | Not loaded. Drop or wire to auto-rejoin invis. |
| `page_length` | 🔶 | Not loaded — runtime defaults all pagers. Drop or wire to `set page <N>`. |
| `olc_zones` | 🔶 | Not loaded. The OLC permission system uses `permissions` + grants. Drop. |
| `race_type`, `player_class` | 🗑️ | Duplicates `race` / `class_id`. **Action items #3, #4.** |
| `height`, `weight` | 🔌 | Schema columns not yet loaded. Today the spawn path rolls fresh via `RaceCatalog::random_{height,weight}` from gender-resolved bands into a `BodyMetrics` component. When persistence is wanted, swap the spawn path to prefer the row values and fall back to the random roll. |
| `base_height`, `base_weight`, `base_size`, `current_size` | 🗑️ | **Drop.** **Action item #5.** |
| `average_stats` | 🗑️ | **Drop.** **Action item #6.** |
| `composition` | 🗑️ | Drops with `Composition` enum. **Action item #15.** |
| `deletion_reason`, `deleted_at` | 🏗️ | Soft-delete columns; Muditor. Loader filters by `deleted_at IS NULL` (added recently — verify). |
| `created_at`, `updated_at` | 🏗️ | Muditor. |

### CharacterAbilities

Schema at `schema.prisma:301-312`. Rust loader: `mud-db/src/character_abilities.rs::CharacterAbilityRow`.

| Field | Status |
|---|---|
| `ability_id`, `known`, `proficiency` | ✅ |
| `last_used` | 🔶 | Not loaded; ambient tracking field. Drop or wire to cooldown logic. |

### CharacterAliases

Schema at `schema.prisma:673-681`. Rust loader: `mud-db/src/character_aliases.rs::CharacterAliasRow`. All fields ✅.

### CharacterEffects

Schema at `schema.prisma:437-454`. **No dedicated `mud-db` module.** Runtime persists effects through `Characters.effect_instances` (JSON blob), not this table. Decide: migrate to normalized table (queryable, but doesn't gain much for transient session state) or drop.

### CharacterItems

Schema at `schema.prisma:507-531`. Rust loader: `mud-db/src/character_items.rs::CharacterItemRow` + diff-write `save_inventory_diff`.

| Field | Status | Notes |
|---|---|---|
| `id`, `character_id`, `object_zone_id`, `object_id`, `container_id`, `equipped_location`, `charges`, `liquid_remaining`, `liquid_type` | ✅ | Round-tripped. |
| `condition`, `custom_name`, `custom_examine_description`, `custom_values` (JSON), `instance_flags`, `liquid_effects`, `liquid_identified` | 🔶 | Loaded into schema; not consumed at runtime. **Action item #35.** Preserved on save (UPDATE leaves them untouched per character_items.rs:10). |

### Races (`Races`)

Schema at `schema.prisma:1064-1113`. Rust loader: `mud-db/src/races.rs` — `list_all` materializes the full Race row into `mud-world::RaceCatalog`; the narrow `list_default_sizes` / `list_start_rooms` helpers are kept for callers that only need those mappings.

| Field | Status | Notes |
|---|---|---|
| `race`, `default_size`, `start_room_zone_id`, `start_room_id` | ✅ | Narrow lookups + full row in `RaceCatalog`. |
| `name`, `plain_name`, `keywords`, `playable`, `humanoid`, `magical`, `race_align`, `default_alignment`, `focus_bonus`, `default_lifeforce`, `male_*` / `female_height_*` / `male_*` / `female_weight_*`, `max_strength`/`dexterity`/`intelligence`/`wisdom`/`constitution`/`charisma`, `exp_factor`, `hp_factor`, `hit_damage_factor`, `damage_dice_factor`, `copper_factor`, `enter_verb`, `leave_verb`, `resistances` (JSON) | ✅ | `RaceCatalog` hydrates full row at boot. Stat caps drive `cmd_train` clamp + creation rolls; factors wired through XP / HP / damage / coin paths; verbs wired through `cmd_move`; size + lifeforce surfaced on `cmd_examine`; race resistances fold into the player `Resistances` component at `spawn_player`. |
| `default_composition` | 🗑️ | **Action item #15.** |
| `created_at`, `updated_at` | 🏗️ | |

### RaceAbilities

Schema at `schema.prisma:1115-1126`. Rust loader: `mud-db/src/race_abilities.rs::RaceAbilityRow`. All fields ✅.

### RaceSpellSlotBonus

Schema at `schema.prisma:1130-1140`. **Not loaded.** Wire when the spell-slot pool system actually gates on race bonuses (today only `SpellSlotProgression` + `ClassAbilityCircles` are consumed).

### RaceEffects

Schema at `schema.prisma:492-505`. **No dedicated `mud-db` module.** Loaded inline if at all. Decide: wire (junction-table CRUD per Phase 3) or drop.

### Effects (`Effect`)

Schema at `schema.prisma:99-145`. Rust loader: `mud-db/src/effects.rs::Effect`.

| Field | Status |
|---|---|
| `id`, `name`, `description`, `effect_type`, `tags`, `presence_override`, `default_params`, `prevents_speaking`, `prevents_casting`, `prevents_movement`, `on_apply`, `on_tick`, `on_remove` | ✅ |
| `param_schema` (JSON Schema) | 🏗️ | Muditor validation. |
| `category_id` → `ToolboxCategory` | 🏗️ | Muditor toolbox display only. |
| `delay_seconds`, `tick_interval_sec` | 🔌 | Not loaded. **Action item #30.** |

### ToolboxCategory

Schema at `schema.prisma:89-97`. 🏗️ Muditor-only.

### Ability (`Ability`)

Schema at `schema.prisma:20-87`. Rust loader: `mud-db/src/abilities.rs::AbilityRow` (16 columns).

| Field | Status | Notes |
|---|---|---|
| `id`, `name`, `plain_name`, `description`, `ability_type` | ✅ | Identity. |
| `violent`, `combat_ok`, `in_combat_only`, `cast_time_rounds`, `cooldown_ms`, `is_area`, `min_position`, `target_scope`, `is_magical` | ✅ | Casting gates. |
| `sphere`, `damage_type` | ⚠️ | Display only on cast/chant listing; not wired to mitigation yet. |
| `notes`, `tags` | ❌ | Not loaded. Decide: wire for builder-side search or drop. |
| `lua_script` | ❌ | Per-ability custom Lua hook. Wire when scripted abilities land. |
| `pages`, `memorization_time`, `quest_only`, `humanoid_only`, `is_toggle`, `contested_visibility`, `visibility_check` | ❌ | Spell-system metadata; not yet loaded. Decide per-field. |
| `school_id` → `AbilitySchool` | ❌ | Not loaded. |
| `classes` (JSON) | 🗑️ | Duplicates `ClassAbilities`. **Action item #10.** |
| `created_at`, `updated_at` | 🏗️ | |

### AbilitySchool

`schema.prisma:161-166`. 🏗️ Muditor catalog — `Ability.schoolId` is the only consumer, and `mud-db` doesn't load `school_id`.

### AbilityComponent / AbilityDamageComponent / AbilityEffect / AbilityMessages / AbilityRestrictions / AbilitySavingThrow / AbilityTargeting

All loaded by dedicated modules in `mud-db/src/`. Status summary:

| Table | Loaded fields | Status |
|---|---|---|
| `AbilityComponent` (reagents) | `id`, `ability_id`, `object_id`, `consumed`, `required` | ⚠️ — loaded into catalog; reagent gating not yet enforced at cast time. |
| `AbilityDamageComponent` (multi-element damage) | `ability_id`, `element`, `damage_formula`, `percentage`, `sequence` | ⚠️ — loaded; per-element resistance routing pending. |
| `AbilityEffect` (effects per ability) | `ability_id`, `effect_id`, `override_params`, `order`, `trigger`, `chance_pct`, `condition` | ⚠️ — loaded; full pipeline interprets `override_params` for some abilities. |
| `AbilityMessages` (templated strings) | All 14 message columns | ✅ — rendered. |
| `AbilityRestrictions` (requirements JSON) | `ability_id`, `requirements` | ⚠️ — only the `message` strings are rendered today; `type` fields ignored. |
| `AbilitySavingThrow` | `ability_id`, `save_type`, `dc_formula`, `on_save_action` | ⚠️ — only 2 rows exist (`BASH`, `TRIP_UP`); evaluator handles those. Promote `on_save_action` JSON → `SaveResult` enum. **Action item promote-2.** |
| `AbilityTargeting` | `ability_id`, `valid_targets`, `scope`, `scope_pattern`, `max_targets`, `range`, `require_los` | ⚠️ — 9/408 abilities have rows; runtime fall-through covers the rest. |
| `AbilityRestrictions.custom_requirement_lua` | — | ❌ — not loaded. Wire if dynamic gating lands. |

### ClassAbilities / ClassSkills / ClassAbilityCircles / SpellSlotProgression

Loaded by `mud-db/src/spell_slots.rs::{ClassAbilityRow, ClassSkillRow, ClassCircleRow, SlotProgressionRow}`. All fields ✅.

### CharacterClass

Schema at `schema.prisma:331-360`. Rust loader: `mud-db/src/classes.rs::ClassRow`.

| Field | Status |
|---|---|
| `id`, `name`, `plain_name`, `is_subclass`, `parent_class_id` | ✅ |
| `description`, `hit_dice`, `primary_stat`, `hp_per_level`, `resistances` (JSON) | ✅ — wired through `ClassCatalog::ClassDef` (`hp_per_level` lands on the level-up gain; resistances fold into the player's `Resistances` map at spawn alongside race resistances). `hit_dice` / `primary_stat` / `description` round-trip and are ready for future consumers. |
| `created_at`, `updated_at` | 🏗️ |

### LevelDefinition

Schema at `schema.prisma:2794-2807`. Rust loader: `mud-db/src/levels.rs::LevelRow`.

| Field | Status |
|---|---|
| `level`, `name`, `exp_required`, `hp_gain`, `stamina_gain`, `is_immortal` | ✅ |
| `permissions` (Permission[]) | ❌ — not loaded. Wire if level-grant permissions are data-driven. |
| `created_at`, `updated_at` | 🏗️ |

### Users (`Users`)

Schema at `schema.prisma:1517-1549`. Rust loader: `mud-db/src/users.rs::User`.

| Field | Status |
|---|---|
| `id`, `email`, `display_name`, `password_hash`, `role`, `failed_login_attempts`, `locked_until` | ✅ |
| `last_login_at`, `reset_token`, `reset_token_expiry`, `last_failed_login` | ⚠️ | `last_failed_login` is written by `record_failed_login` but never read by Rust. Reset-token columns are Muditor-only (password reset flow). |
| `preferences` (JSON) | 🏗️ | Muditor. |
| `account_wealth` | ✅ | Account-shared bank balance, loaded into the `AccountWealth` component at spawn (Wave 5.1, 2026-05-12). Wired by `account_balance` / `account_deposit` / `account_withdraw`; cross-character sync via `account_bank::fanout_account_wealth`. Save path: `users::save_account_wealth` in `save_player`. |
| `created_at`, `updated_at`, `deleted_at`, `deletion_reason` | 🏗️ | Muditor. |

### UserGrants

Schema at `schema.prisma:1551-1567`. 🏗️ Muditor permission system; not loaded by `mud-db`.

### BanRecords

Schema at `schema.prisma:401-418`. Rust loader: `mud-db/src/bans.rs::ActiveBanRow`.

| Field | Status |
|---|---|
| `reason`, `banned_by`, `banned_at`, `expires_at` | ✅ |
| `id`, `user_id`, `unbanned_at`, `unbanned_by`, `active` | ⚠️ | Used in WHERE / INSERT / UPDATE but not loaded into the read struct. |

### AuditLogs

Schema at `schema.prisma:389-399`. Rust loader: `mud-db/src/audit.rs::record` — **write-only**. The schema's `old_values` is never set, but `new_values` carries args JSON. ⚠️ partial-use is intentional.

### ChangeLogs

Schema at `schema.prisma:420-434`. 🏗️ Muditor-only audit table. Not loaded.

### AccountItems

Schema at `schema.prisma:1571-1588`. ✅ Loaded by `mud-db/src/account_items.rs` (Wave 5.2, 2026-05-12). Account-shared item storage, surfaced by the `chest` / `chest_deposit` / `chest_withdraw` async commands.

| Field | Status |
|---|---|
| `id`, `user_id`, `slot`, `object_zone_id`, `object_id`, `quantity`, `custom_data`, `stored_by_character_id`, `stored_at` | ✅ |

`custom_data` carries a small JSON envelope (`charges`, `liquid_remaining`, `liquid_type`, `light_remaining`) so per-instance state survives the chest round-trip. SOULBOUND / NO_DROP gates refuse deposit. The `quantity` column accepts >1 for future stack support; v1 always deposits 1.

### AccountMail

Schema at `schema.prisma:2627-2660`. Rust loader: `mud-db/src/mail.rs` (`MailRow` + send/inbox/mark_read/soft_delete). All used fields ✅.

| Field | Status |
|---|---|
| `id`, `sender_user_id`, `recipient_user_id`, `subject`, `body`, `sent_at`, `read_at`, `is_deleted` | ✅ |
| `is_broadcast` | ❌ | Not loaded — only set on insert when `recipient_user_id IS NULL`. Drop the column if broadcast mail isn't a separate code path. |
| `created_at`, `updated_at` | 🏗️ | |

### PlayerMail

Schema at `schema.prisma:2569-2624`. **Not loaded by `mud-db`.** Per 2026-04 mail rewrite, runtime uses `AccountMail` only. **Action item #22.**

### TellMessage

Schema at `schema.prisma:2669-2680`. Rust loader: `mud-db/src/tell_messages.rs::TellMessageRow`. All fields ✅.

### Clan / ClanMember

Schema at `schema.prisma:2684-2709`. Rust loader: `mud-db/src/clans.rs`. All schema fields used. ✅.

### Achievement / CharacterAchievement

Schema at `schema.prisma:2733-2765`. Rust loader: `mud-db/src/achievements.rs::{AchievementRow, CharacterAchievementRow}`.

| Field | Status |
|---|---|
| `Achievement.id`, `code`, `title`, `description`, `category`, `hidden`, `sort_order` | ✅ |
| `Achievement.created_at`, `updated_at` | 🏗️ |
| `CharacterAchievement.character_id`, `achievement_id`, `progress` | ✅ |
| `CharacterAchievement.unlocked_at` | 🔶 | Stamped on insert but never read back. Drop or surface in `achievements` listing. |

### GameConfig

Schema at `schema.prisma:2773-2791`. Rust loader: `mud-db/src/game_config.rs::GameConfigRow`. Loaded into `RuntimeConfig` resource and read via `get_i32/get_bool` in `mud-server::main` and `regen`.

| Field | Status |
|---|---|
| `category`, `key`, `value`, `value_type`, `description` | ✅ |
| `min_value`, `max_value`, `is_secret`, `restart_req` | 🏗️ | Muditor admin UX. |

### LoginMessage

Schema at `schema.prisma:2894-2906`. Rust loader: `mud-db/src/login_message.rs::LoginMessageRow`. Loaded into `LoginMessageCatalog`; consumed by `mud-server::login::login_message_bytes`.

| Field | Status |
|---|---|
| `stage`, `variant`, `message` | ✅ |
| `is_active` (filter) | ✅ | Used in WHERE. |
| `id`, `created_at`, `updated_at` | 🏗️ | |

### SystemText

Schema at `schema.prisma:2850-2864`. Rust loader: `mud-db/src/system_text.rs::SystemTextRow`. Loaded into a runtime resource; consumed by `mud-server::commands::info::system_text_with_fallback`.

| Field | Status |
|---|---|
| `key`, `category`, `title`, `content`, `min_level`, `is_active` | ✅ |
| `id`, `created_at`, `updated_at` | 🏗️ | |

### Command

Schema at `schema.prisma:2811-2831`. **Not loaded by `mud-db`.** Schema says "synced from C++ on startup," but the Rust runtime uses the `inventory::submit!` static command registry in `mud-server`. Drop or wire as the canonical source of truth.

### HelpEntry

Schema at `schema.prisma:366-387`. ✅ **Loaded** via `mud-db/src/help.rs::HelpEntryRow` (`list_all`). Hydrated at boot into `mud_world::HelpCatalog` (keyword index + per-id map). `cmd_help` (`mud-server/src/commands/info.rs`) falls through to the catalog after command-registry and social lookups, with case-insensitive exact-keyword match plus title-prefix fallback. `min_level` gates visibility the same way `SystemTexts::content` does.

| Field | Status |
|---|---|
| `id`, `keywords`, `title`, `content`, `min_level`, `category`, `usage`, `duration`, `sphere` | ✅ |
| `source_file`, `created_at`, `updated_at` | 🏗️ Muditor metadata |

### EntityVariables

Schema at `schema.prisma:1373-1388`. Rust loader: `mud-db/src/entity_variables.rs::list_all` (boot-time hydration). Cache: `mud-world::EntityVariableCache`. Lua API: `self:setvar`/`:getvar`/`:clearvar` on both `LuaActor` (mob/object kinds) and `LuaRoom`. Flush: `mud-server::entity_vars::entity_var_flush_tick` every 100 ticks (10s) via `upsert_many` + per-key `delete`. ✅ Used.

### Triggers (`Triggers`)

Schema at `schema.prisma:1395-1432`. Rust loader: `mud-db/src/triggers.rs::TriggerRow`.

| Field | Status |
|---|---|
| `zone_id`, `id`, `name`, `attach_type`, `num_args`, `arg_list`, `commands`, `flags` | ✅ |
| `mob_zone_id`, `mob_id`, `object_zone_id`, `object_id` | 🗑️ | Direct FKs superseded by junction tables. **Action item #1.** |
| `variables` (JSON) | 🗑️ | **Action item #2.** |
| `needs_review`, `syntax_error` | 🏗️ | Muditor validation tracking. |
| `created_at`, `updated_at`, `created_by`, `updated_by` | 🏗️ | |

### MobTriggers / ObjectTriggers / RoomTriggers

All three junction tables: Rust loader `mud-db/src/triggers.rs::{MobTriggerLink, ObjectTriggerLink, RoomTriggerLink}`. All composite-key fields ✅. `createdAt` 🏗️.

### ScriptErrorLog

Schema at `schema.prisma:1483-1498`. Rust loader: `mud-db/src/script_errors.rs::record` — write-only. ⚠️ partial by design (Muditor reads).

### Shops (`Shops`)

Schema at `schema.prisma:1354-1383`. Rust loader: `mud-db/src/shops.rs::Shop`.

| Field | Status |
|---|---|
| `zone_id`, `id`, `keeper_zone_id`, `keeper_id`, `buy_profit`, `sell_profit` | ✅ |
| `temper` | ❌ | Not loaded. Drop unless shopkeeper mood system lands. |
| `noSuchItemMessages`, `doNotBuyMessages`, `missingCashMessages`, `buyMessages`, `sellMessages` | ❌ | Not loaded. Default messages compiled in. Wire for builder-customizable shop dialogue or drop. |
| `flags` (`ShopFlag[]`), `tradesWithFlags` (`ShopTradesWith[]`) | ❌ | Not loaded. Wire for `WILL_FIGHT` / `USES_BANK` / trade-restriction logic, or drop. |
| `created_at`, `updated_at`, `created_by`, `updated_by` | 🏗️ | |

### ShopItems / ShopMobs / ShopAccepts

Rust loader: `mud-db/src/shops.rs::{ShopItem, ShopMob, ShopAccept}`.

| Field | Status |
|---|---|
| All shared FK + amount/price columns | ✅ |
| `spawn_chance`, `visibility_requirement`, `purchase_requirement` (on ShopItems, ShopMobs, and ShopAbilities) | ❌ | Lua-gated stock/visibility. Wire if/when shops grow rules; or drop. |

### ShopHours / ShopRooms / ShopAbilities

`schema.prisma:1275-1352`. Not loaded. **Drop.** *Action item #18.*

### Quests / QuestPhases / QuestObjectives / QuestPrerequisites / QuestRewards / CharacterQuests / CharacterQuestObjectives

Loaded by `mud-db/src/quests.rs` + `mud-db/src/quest_objectives.rs`. See per-table summary:

| Table | Loaded fields | Status |
|---|---|---|
| `Quests` | `zone_id`, `id`, `name`, `plain_name`, `description`, `short_description`, `min_level`, `max_level`, `repeatable`, `shareable`, `hidden`, `auto_accept` | ✅ |
| `Quests` (trigger columns) | `triggerType`, `triggerMobZoneId`, `triggerMobId`, `triggerLevel`, `triggerItem*`, `triggerRoom*`, `triggerAbilityId`, `triggerEventId`, `timeLimitMinutes`, `cooldownMinutes`, `exclusiveGroup`, `availabilityRequirement` | ✅ 2026-05-12 — all loaded by `mud_db::quests::QuestRow` + per-trigger `list_by_trigger_*` queries. Wave 4.1-4.5 wires dispatchers, expiry sweeper, exclusive-group gate, availability Lua. |
| `QuestPhases` | `id`, `name`, `order` (via join) | ✅ |
| `QuestPhases.description` | ✅ 2026-05-12 — loaded via `ObjectiveListingRow.phase_description`, rendered beneath each phase header in `cmd_quests`. |
| `QuestObjectives` | `quest_zone_id`, `quest_id`, `phase_id`, `id`, `objective_type`, `scope`, `player_description`, `show_progress`, `required_count`, all `target_*` and `deliver_to_*` columns | ✅ |
| `QuestObjectives.internalNote`, `luaExpression` | ✅ 2026-05-12 — `internal_note` surfaced to Builder+ in `cmd_quests`. `lua_expression` drives CUSTOM_LUA objectives via `quest_custom_lua_tick`/`drain`. |
| `QuestPrerequisites` | `prerequisite_quest_zone_id`, `prerequisite_quest_id`, `require_completion` | ✅ |
| `QuestRewards` | `reward_type`, `amount`, `object_zone_id`, `object_id`, `ability_id`, `quantity`, `choice_group` (WHERE filter) | ✅ |
| `QuestRewards.condition` (Lua) | ✅ 2026-05-12 — loaded on `QuestRewardRow`. Auto-grant path skips conditional rewards (no world from async); `qreward` claim path evaluates via `eval_quest_availability`. |
| `CharacterQuests` | `id`, `character_id`, `quest_zone_id`, `quest_id`, `status`, `accepted_at`, `completed_at`, `completion_count`, `current_phase_id` | ✅ |
| `CharacterQuests.variables` (JSON), `expiresAt` | ✅ 2026-05-12 — both loaded. `variables` consumed by `qreward` claim tracking + CUSTOM_LUA `quest_vars_json`. `expires_at` flipped to FAILED by `quest_sweep_tick` → `fail_expired_quests`. |
| `CharacterQuestObjectives` | All fields | ✅ |

### QuestDialogue / DialogueTrees / DialogueNodes / DialogueResponses

`schema.prisma:3393-3475`. ✅ 2026-05-12 — all four tables loaded by `mud_db::dialogue::list_trees`/`list_nodes`/`list_responses`/`list_quest_dialogues`; the runtime catalog (`quest_dialogue::DialogueCatalog`) hydrates at boot. `cmd_ask` walks the catalog (mid-tree via `try_advance_active_tree`, first-utterance via `dispatch_dialogue_attempt`). EXACT / CONTAINS / STARTS_WITH / ANY_OF supported; REGEX falls back to CONTAINS — see [parking-lot.md](./parking-lot.md).

### Liquid

`schema.prisma:3323-3346`. Rust loader: `mud-db/src/liquids.rs::LiquidRow` + `list_all`. Loaded at boot into the `LiquidCatalog` resource (lookup by alias / id, with a water-shaped fallback for unknown aliases). Consumed by the drink path for per-swig hunger/thirst/drunk deltas, taste/examine renderers (color_desc when unidentified, name+description when identified), and pour/fill for alias canonicalization on container transfers. ✅

### ConsumableEffect

`schema.prisma:3619-3641`. Rust loader: `mud-db/src/consumable_effects.rs::ConsumableEffectRow` + loader at `mud-world::loader.rs:812`. Fields `effect_id`, `chance`, `level`, `duration`, `liquid_id`/`object_zone_id`/`object_id` (FK filters) ✅.

### Player Housing — `PlayerHouse`, `PlayerHouseRoom`, `PlayerHouseExit`, `PlayerHouseItem`, `PlayerHouseGuest`

`schema.prisma:3648-3740`. Rust loader: `mud-db/src/housing.rs` (5 row structs + CRUD). Consumed by `mud-server::commands::admin_management` (hgrant / hrevoke / hinspect / etc.) and core home/guest commands.

| Table | Used fields | Notes |
|---|---|---|
| `PlayerHouse` | All 6 | ✅ |
| `PlayerHouseRoom` | `id`, `house_id`, `local_index`, `name`, `description`, `is_peaceful`, `base_light_level`, `capacity` | ✅ |
| `PlayerHouseRoom.sector`, `created_at`, `updated_at` | 🔶 | Not loaded. `sector` defaults to `STRUCTURE` per schema; drop unless variation is needed. |
| `PlayerHouseExit` | `id`, `from_room_id`, `to_room_id`, `direction` | ✅ |
| `PlayerHouseItem` | `id`, `room_id`, `object_zone_id`, `object_id`, `condition` | ✅ |
| `PlayerHouseItem.custom_values`, `placed_at` | 🔶 | Not loaded. Drop or wire. |
| `PlayerHouseGuest` | All 4 | ✅ |

### SpawnConditions

`schema.prisma:1385-1393`. **Not loaded.** Drop. **Action item #23.**

### CombatMessage / PositionData / PositionMessage / SystemMessage

`schema.prisma:3095-3228`. **None loaded.** Decide: wire (parallel to `SystemText`) for builder-authored hit/miss/posture flavor, or drop. **Action items #25-26.**

### Events

`schema.prisma:1591-1613`. **Not loaded.** Wire for seasonal content or drop. **Action wire-7.**

### Social

`schema.prisma:1642-1668`. Rust loader: `mud-db/src/socials.rs::SocialRow`.

| Field | Status |
|---|---|
| `id`, `name`, `hide`, `char_no_arg`, `others_no_arg`, `char_found`, `others_found`, `vict_found`, `not_found`, `char_auto`, `others_auto` | ✅ |
| `min_victim_position` | 🔶 | Not loaded. Wire or drop. |
| `created_at`, `updated_at` | 🏗️ | |

### Board / BoardMessage / BoardMessageEdit

`schema.prisma:2953-3004`. Rust loader: `mud-db/src/boards.rs::{Board, BoardMessage}` + CRUD. All fields used **except**:

| Field | Status |
|---|---|
| `Board.privileges` (JSON) | 🔶 | Not loaded. Board access control runs through `Permission` flags. Drop or wire. |
| `BoardMessage.created_at`, `updated_at` | 🏗️ | |
| `BoardMessageEdit` (entire table) | ⚠️ | Written by `update_message` but never read back. Verify Muditor reads this — if not, drop. |

### Report

`schema.prisma:3058-3087`. Rust loader: `mud-db/src/reports.rs::submit` — write-only. ⚠️ Muditor reads.

### DiscordLink / GoogleLink / DiscordConfig

`schema.prisma:3011-3052`. ✅ 2026-05-12 (Wave 6.1–6.3).
- `mud_db::discord_links::{for_user, for_discord_id, link, unlink, mark_verified}` — bot ingress + in-game `discord link` / `discord unlink`. Two-step verification: `PendingDiscordLinks` runtime resource holds the 6-digit code (10-min TTL).
- `mud_db::google_links::{for_user, link, unlink}` — Muditor OAuth callback writes; in-game `account` reads.
- `mud_db::discord_config::get` — singleton row at PK 1, published as `DiscordConfigCatalog` resource at boot. `can_send_gossip` / `can_send_admin` / `can_send_announcement` gate outbound traffic destination tags. Bot itself runs out-of-process (Muditor-side) — see parking lot.
- Unified `account` command (info.rs::cmd_account) renders email + role + roster + Discord (verified/unverified) + Google.
- Live-DB tests: `discord_link_round_trip`, `google_link_round_trip`, `discord_config_get_shape`.

### Character name approval (replaces `LoginRequests`)

`LoginRequests` table + `LoginRequestStatus` enum + four `LoginStage.LOGIN_APPROVAL_*` variants **dropped** 2026-05-12. The login-time approval gate was the wrong model — it blocked play entirely on every login, including for established characters whose names had been live for years. Replaced by a per-character one-shot name-approval gate on the `Characters` table.

- New column: `Characters.name_approved Boolean @default(true)`. Existing characters auto-grandfathered.
- Runtime gate: `security.login_approval_required` replaced by `social.name_approval_required` (default OFF) in GameConfig. When ON, char-creation inserts the new character at `name_approved = false`.
- `mud-world::NameApprovalPending` marker component attached at spawn iff column is `false`. Player can play (move / look / fight); social commands (`tell`, `reply`, `say`, `whisper`, `gsay`, `gossip`, `music`, `shout`, `qsay`, `ctell`, `invite`) refuse via the shared `commands::name_approval_gate` helper.
- Staff resolution: Immortal+ `approve_name <character>` (keep the name, flip column + drop live marker + DM player) or `reject_name <character> <new>` (force-rename + auto-approve; refuses while target is online). Players run `name_status` to self-diagnose.
- `mud_db::characters::set_name_approved` + `CharacterRow.name_approved` field carry the persistence. `NewCharacter.name_approved` chosen by the runtime at create time.
- Live-DB tests: `character_name_approval_round_trip`, `character_name_approval_defaults_true`. Unit tests: `name_approval_gate_blocks_when_marker_present`, `name_approval_gate_clears_when_marker_removed`.

### ShapechangeForm / AbilityShapechangeForm

`schema.prisma:3746-3774`. **Not loaded.** **Action item #20.**

### DeploymentPackage / DeploymentChange

`schema.prisma:3789-3819`. **Not loaded.** **Action item #21.**

### PlayerToggle

`schema.prisma:2868-2883`. **Not loaded.** **Action item #19.**

---

## Enums — declared but not read

These enums are declared in `schema.prisma` but never appear in any Rust `sqlx::Type` decoder or `query_as` cast. Each suggests a column slated for drop.

| Enum | Used by columns | Status |
|---|---|---|
| `Composition` | `Mobs.composition`, `Races.defaultComposition` | 🗑️ Action item #15 |
| `MagicAffinity` | `Room.magicAffinity` | 🗑️ Action item #16 |
| `Stance` | `Mobs.stance` | 🗑️ Action item #17 |
| `PositionMechanic` | `Room.requiredMechanic`, `PositionData.mechanic` | 🗑️ Action item #27 |
| `ApplyType` | none — purely Muditor-facing | 🏗️ |
| `HitType` | `CombatMessage.hitType` | 🗑️ if combat messages dropped |
| `SaveType` | `AbilitySavingThrow.saveType` | ✅ (loaded as text) |
| `SaveResult` | declared, never used as a column type | 🔌 — promote `AbilitySavingThrow.onSaveAction` to this enum |
| ~~`LoginRequestStatus`~~ | ~~`LoginRequests.status`~~ | 🗑️ enum + parent table dropped 2026-05-12 (replaced by `Characters.name_approved`). |
| `DeploymentStatus` | `DeploymentPackage.status` | 🗑️ with action item #21 |
| `ClanRank`, `ToggleCategory`, `ConfigValueType`, `SystemTextCategory`, `LoginStage`, `CommandCategory`, `ShopFlag`, `ShopTradesWith`, `Hemisphere`, `Climate`, `ResetMode`, `QuestTriggerType`, `QuestObjectiveScope`, `QuestObjectiveType`, `QuestRewardType`, `QuestStatus`, `DialogueMatchType`, `LifeForce`, `ObjectType`, `ObjectFlag`, `ObjectRestriction`, `WearFlag`, `ExitFlag`, `ExitState`, `Direction`, `Sector`, `Race`, `RaceAlign`, `Alignment`, `Size`, `Gender`, `MobRole`, `ProtectedKind`, `MobTrait`, `MobBehavior`, `MobProfession`, `MovementMode`, `Position`, `Permission`, `PlayerFlag`, `ElementType`, `SpellSphere`, `TargetType`, `TargetScope`, `SkillCategory`, `SkillType`, `StackingRule`, `EntityType`, `ScriptType`, `TriggerFlag`, `UserRole`, `GrantResourceType`, `GrantPermission`, `ItemInstanceFlag`, `AchievementCategory`, `ReportType`, `ReportStatus` | various | ✅ or 🏗️ (audited above) |

---

## Methodology

This document is the result of:

1. Reading every model in `muditor/packages/db/prisma/schema.prisma` (3,819 lines, 118 models).
2. Reading every file under `fierymud-rs/crates/mud-db/src/` (57 files) to identify the columns each Rust loader actually SELECTs.
3. Grepping `mud-server`, `mud-world`, `mud-script` for downstream usage of each loaded field — distinguishing actively consumed from loaded-into-a-struct-then-ignored.
4. Cross-referencing with [`schema-reconciliation.md`](./schema-reconciliation.md) for already-decided drops.

A column is **loaded** if a `mud-db` query SELECTs it. **Consumed** means a downstream call site reads from the resulting Rust struct field. Both conditions must hold for ✅.

When this document and `schema-reconciliation.md` disagree, prefer this document — it incorporates the more recent audit pass.
