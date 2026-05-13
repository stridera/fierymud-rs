# Parking Lot — Items Needing User Feedback

**Purpose:** decisions / edge cases / surprises that surface during the drop-wire migration and don't block progress. The user reviews these on regular check-ins.

**Convention:** add new entries at the bottom with a date stamp. Move to "Resolved" once the user weighs in.

---

## Open

### 2026-05-12 · Characters combat-col defaults / backfill strategy

**Context:** Combat redesign drops `Characters.{hit_roll, damage_roll, armor_class}` and adds `accuracy`, `attack_power`, `spell_power`, `penetration_flat/percent`, `evasion`, `armor_rating`, `damage_reduction_percent`, `soak`, `hardness`, `ward_percent`, `perception`, `concealment`, `resistances` (JSON) — mirroring `Mobs`.

**Decision needed:** legacy-imported player characters (in `fierylib`) currently have legacy `hit_roll`/`damroll`/`ac` from CircleMUD pfiles. After the migration:

- **Option A** — backfill via the same conversion fierylib uses for mobs (`combat_formulas.convert_legacy_to_modern_stats`). Preserves player progression value.
- **Option B** — start fresh at zeros, let class/race progression bring stats up over time. Slight regression for imported chars; clean break.
- **Option C** — derive at runtime from class/race/level (no per-character persisted combat stats). Most elegant but radically different from `Mobs` shape.

**Proceeding default:** Option B (zeros). Test seeder characters are the main population today; legacy player imports are rare. If this turns out to matter, easy to swap to A later.

### 2026-05-12 · Mobs `hp_dice_*` / `damage_dice_*` — drop or keep?

The original audit recommended dropping these along with `hit_roll`/`armor_class` as "replaced by combat redesign." But the in-flight schema diff keeps them, and `MobProto::rolled_hp()` / `avg_damage()` rely on them to produce per-spawn HP and base damage.

**Proceeding default:** keep. They are the spawn-time HP/damage shape, orthogonal to `accuracy`/`evasion`/`armor_pct`. If the redesign wants a flat `max_hp` column instead, that's a separate migration.

### 2026-05-12 · Combat balance review (Wave 3.13)

Per the prior agent's notes:
- `hit_chance_pct` formula in `combat.rs` is the proper d100-d100 contest from `docs/design/combat.md`, but the magnitudes may differ from spec. Worth a balance pass — compare expected hit rates against legacy + combat.md targets.
- `attack_power` is a flat damage bonus in current code; combat.md describes it as a % multiplier. Either path is defensible; need a decision.
- `crit_chance` is hardcoded to 5 across the board. Consider promoting to a schema column (per-mob/per-class crit tuning).
- `posture_evasion_penalty` values (0/10/20/25/30) need playtesting — were 0/2/4/5/6 against AC, which is a different scale.

### 2026-05-12 · Room flag wired but consumer pending: ArenaRoom (PK opt-in)

**Description:** `Room.is_arena = true` is loaded as the `ArenaRoom` marker on the room entity, and `cmd_look` displays a "Combat is welcome here." flavor line when set. The legacy semantic is "PK is allowed between non-consenting players in an arena" — but the runtime has no consent gate yet (PvP is currently allowed everywhere except PeacefulRoom). The marker is stored faithfully, the display works, but the "override PK refusal" branch has no companion gate to override.

**What to do when consumer lands:** once a PK opt-in gate is introduced (e.g. attacking a Player without `PlayerFlag::PkEnabled` is refused), add a "...unless `ArenaRoom`" branch right next to the PeacefulRoom check in `commands/combat.rs::cmd_attack` and the helper aggro paths in `commands.rs` (`engage_combat`, `try_engage_remembered_mob`, `try_engage_aggressive_mob`).

### 2026-05-12 · Room flag wired but consumer pending: GuildhallRoom (class trainers)

**Description:** `Room.is_guildhall = true` lands as `GuildhallRoom` and shows a "This is a guild hall." `look` line. The legacy semantic is "class trainers only operate in their matching guildhall" — but no class-trainer system exists yet on the fierymud-rs side. Marker stored; consumer pending.

**What to do when consumer lands:** when class-trainer command(s) ship (`practice` / `train` for class skills), the gate is "trainer mob refuses to teach outside a GuildhallRoom that matches their class affiliation." Pair the check with a per-trainer-class proto field once authored. For now, the marker is also a visible cue for builders that a room is intended as a guildhall.

### 2026-05-12 · Room flag wired but consumer pending: NoPortalsRoom

**Description:** `Room.allows_portals = false` lands as `NoPortalsRoom`. Legacy semantic is "portal / moonwell creation refuses to link this room as endpoint." No portal/moonwell spell exists yet in fierymud-rs. Marker stored; consumer pending.

**What to do when consumer lands:** the gate sits in the portal-creation path — when a `portal` / `moonwell` ability resolves its endpoints, check `world.get::<NoPortalsRoom>(source).is_some() || world.get::<NoPortalsRoom>(destination).is_some()` and refuse with "the room rejects your portal's anchor." Mirror the NoTeleportRoom gate's wording for consistency.

### 2026-05-12 · Object flags wired but consumers pending — wave 2.B

`ObjectFlag` and `ObjectRestriction` enums are now in `mud-db` and flow
through to per-instance `ObjectFlags(Vec<…>)` / `ObjectRestrictions(Vec<…>)`
components at every spawn site (reset pass, mob equipment, contents,
inventory rehydration, admin loadobj, corpse snapshot). The following
flags carry through to the entity but no consumer path exists yet —
stored faithfully so the gate is one match-arm away when the matching
system lands.

| Flag / Restriction | Pending consumer |
|---|---|
| `PERMANENT` | `Object.timer` decay tick (wave 2.C). Today the flag exists on the entity; no decay logic to skip. |
| `TEMPORARY` | Save filter wired in `login::save_player` (snapshot skips TEMPORARY). The complementary "vanish on rent" event hasn't shipped because rent isn't modeled either; the save filter is the actual mechanism for now. |
| `FLOAT` | Affects "drop" behavior in air rooms — but `Sector::Air` rooms don't have a fall-through-on-drop pipeline today. Component stamped, no gate. |
| `BUOYANT` | Boat behavior in water sectors. Today's water rooms don't track surface vs. swimming differently for items. Component stamped, no gate. |
| `VEHICLE` | Mountable / passenger-capable. Mount system exists (`Mounted` / `RiddenBy`) but only mob mounts use it; item mounts (carts, ships) need a separate pickup path. Component stamped, no gate. |
| `DECOMPOSING` | Wired as a cosmetic-only "smells faintly of rot" line in `cmd_examine`. No active decay yet. |
| `NO_BURN` | Wired/skipped because no fire-damage-to-inventory pipeline exists. Inventory items aren't currently damaged by anything; once fire-damage-to-inventory lands, filter the target list with `!has_restriction(item, NoBurn)`. |
| `NO_LOCATE` | `locate` ability doesn't exist yet. When it ships, filter the candidate list with `!has_restriction(item, NoLocate)`. |
| `NO_INVISIBLE` | Object-targeted invisibility spell doesn't exist; invis on objects is currently always proto-set (the `Invisible` flag), never spell-applied. When `invisibility` can target an item, gate the apply with `!has_restriction(item, NoInvisible)`. |

`INVISIBLE` filtering is partially complete: items with the flag are
hidden from `cmd_look` ground listings and `cmd_examine` searches
unless the viewer has `HOLY_LIGHT` (staff vision). A normal player
"detect invisible" effect doesn't exist yet; once it lands, route
through `crate::commands::player_can_see_in_dark` (or a new helper)
so all three call sites stay in sync.

`HUM` is wired as a single ambient "you hear something humming
nearby" line in `cmd_look` — fired regardless of room light level
so blind / pitch-dark observers still get the auditory cue.

### 2026-05-12 · Mob latent components wired, consumers pending — wave 2.L

The Wave 2.L wire landed eight columns from `Mobs` (`size`, `lifeForce`, `damageType`, `move`, `defaultPosition`, `traits`, `movementMode`, `defaultMovementMode`) onto every spawned mob as the components below. Each component is on the entity faithfully; the matching gameplay system isn't all present yet, so the gates are listed here for the next consumer authors to find.

| Component | Wired consumer | Pending consumer |
|---|---|---|
| `Sized(Size)` | `cmd_examine` flavor line ("It is a HUGE creature."), `mstat` readout | `bash` skill (size-disparity refusal) — no bash skill yet. `drag` / `throw` — no skill. `mount` eligibility — Mountable marker handled separately today. |
| `LifeForceTag(LifeForce)` | `cmd_examine` aura lines (undead / celestial / demonic / etc.), `mstat` readout | `detect_undead` / `dispel_undead` / `turn_undead` / `holy_word` ability filters — none of those abilities exist yet. When they ship, query `LifeForceTag(LifeForce::Undead)` to filter the candidate list. |
| `NaturalAttackType(DamageType)` | `combat.rs::apply_swing` swing verb ("The wolf bites you." / "The orc claws you."), `mstat` readout | Optional: future `attack_message` builder-authored variants (`CombatMessage` table) could read the type for selection. |
| `MobTraits(Vec<MobTrait>)` | `cmd_examine` (display), `mstat` readout, `wander_tick` (`MobTrait::Aquatic` gate), spawn-time Mountable inference (`MobTrait::Mount`) | `dispel_illusion` (`MobTrait::Illusion`) — no dispel-illusion ability yet. `PLAYER_PHANTASM` — no faux-player aggro path yet. `SUMMONED` — no `banish` ability yet. `PET` — pet management already keys on `PersistentPet`, but `MobTrait::Pet` could harden that path once command authors layer the trait onto a wider net. |
| `MovementModeTag(MovementMode)` | `cmd_examine` flavor (FLYING / SWIMMING / UNDERWATER / ETHEREAL cues), `mstat` readout | Combat range / dodge / "X soars overhead" rendering — aerial-combat redesign hasn't landed. ETHEREAL pass-through-terrain — no ethereal movement system. UNDERWATER drowning checks — drowning ticks today key on `Posture` only. |
| `MovementPoints { current, max }` | `mstat` readout (via proto field, since the component is optional) | Wander tick should decrement on each move, regen tick should restore — neither is wired yet. Today wander is uncosted; the component exists on mobs with non-zero `move_points` so the consumer code can come later without re-spawning. |

`Posture::from_default_position(proto.default_position)` is invoked at three spawn sites (loader / respawn / summon) so SLEEPING / SITTING / RESTING starting postures take effect immediately; DEAD / GHOST / MORTALLY_WOUNDED / INCAPACITATED / STUNNED legacy variants fall back to STANDING by design.

### 2026-05-12 · Discord bot (Muditor-side) — Wave 6 follow-up

**Context:** Wave 6 wired the DB schema for `DiscordLink` / `GoogleLink` /
`DiscordConfig` so the in-game side reads and writes correctly:
- Players can run `discord link <id>` and the Rust runtime mints a 6-digit
  verification code into the `PendingDiscordLinks` resource (10-min TTL,
  expired by a periodic 30s tick — formerly tied to the
  `login_requests` sweep, which was removed in favor of name-approval).
- `discord unlink` removes the binding.
- `discord_config` is loaded at boot as `DiscordConfigCatalog` with
  `can_send_*` gates.

**Pending (Muditor-side):**
- The actual bot process. The bot must consume the `PendingDiscordLinks`
  resource entries via an admin endpoint (or a polling read of a future
  `pending_discord_verifications` table — TBD shape), match `/verify <code>`
  messages from the gossip channel, and call
  `discord_links::link` + `mark_verified` on success.
- Outbound dispatch — when a gossip / clan / admin broadcast wants to mirror
  to Discord, the runtime today checks `DiscordConfigCatalog.can_send_gossip`
  but has no message queue. Either:
  (a) write to a `discord_outbound_queue` table the bot polls, or
  (b) expose a `/api/admin/discord/queue` endpoint the runtime posts to.
- Google OAuth callback — Muditor handler is the only writer for
  `google_links`. Today the link is created server-side from the OAuth
  `sub` / email; the in-game `account` command displays it. No in-game
  unlink (`account unlink google`) command yet — TBD whether it lives
  alongside `discord unlink` or stays Muditor-only.

<!-- LoginRequests polling-resume + denial/expiry-notice entries —
     RESOLVED 2026-05-12 by replacing the login-time approval flow
     with the per-character name-approval gate; see Resolved below.
     The underlying need (don't kick legit players, just gate names
     that need review) is now met without parking a session at all. -->

<!-- Quest dialogue REGEX matcher — resolved 2026-05-12, see Resolved below. -->

<!-- Quest variable Lua setvar / getvar bindings — resolved 2026-05-12, see Resolved below. -->

<!-- Quest EVENT-trigger dispatcher — resolved 2026-05-12, see Resolved below. -->

<!-- Wave 4.10 conditional rewards — UX polish resolved 2026-05-12, see Resolved below. -->


### 2026-05-12 · Lua `run_room_trigger(zone, id)` — WIRED via deferred queue

**Description (resolved):** Trigger bodies in zones 117 / 123 / 163 / 185 (and a handful of others) call `run_room_trigger(zone, id)` to hand off control between rooms. The binding now enqueues onto the `DeferredRoomTriggerFires` resource (mud-world) and the queue is drained by `triggers::drain_deferred_room_triggers` inside `lua_coroutine_tick`, *outside* the current Lua frame so the mlua re-entrancy guard never trips.

The drain fires every trigger attached to the target room (no event-flag filter), with `self = room` and `actor = caller` (the SelfEntity at the call site, falling back to the room itself if the caller despawned between enqueue and drain). Target rooms that don't resolve via `WorldKeyIndex` are warn-logged and skipped — matches the `get_room(zone, id) → nil` pattern the corpus already tolerates. Chained handoffs (target trigger calls `run_room_trigger` again) are deferred to the next tick rather than looping inside the drain pass.

### 2026-05-12 · Lua `wait_until(hour, minute)` — WIRED at hour granularity

**Description (partially resolved):** Lua `wait_until(hour, minute)` now suspends the coroutine via the same `wait(N)` → `coroutine.yield` mechanism the rest of the host uses, with the seconds-to-wait computed from `MudClock.hour` and the 750-ticks-per-game-hour cadence (75 real seconds per game hour). Same-hour calls wait a full game day so a trigger at midnight doesn't busy-loop.

**Remaining caveat — minute argument is accepted but ignored.** `MudClock` only advances hour-by-hour today (one game hour per 750 real ticks; there is no minute field). The Lua signature keeps the `minute` arg for forward-compatibility, but the wakeup is on the hour boundary regardless. Authored bodies that expect minute-precision scheduling (e.g. "open the gate at 6:30") wake at the start of the named hour (6:00) instead. When `MudClock` gains a `minute` field, the `_seconds_until(h, m)` helper in `mud-script/src/lib.rs` is the only piece that needs to start respecting the second argument — the rest of the path is already minute-aware in shape.

### 2026-05-12 · Wave 7 sweep: equip_apply mapping for modern combat columns (RESOLVED inline)

**Description (resolved):** boot log was warning `ObjectAffect: unsupported APPLY_* location` on `ATTACK_POWER` / `ACCURACY` / `SPELL_POWER` / `EVASION` / `ARMOR_RATING` / `HARDNESS` / `WARD_PCT` — these are the modern combat-redesign stat names that fierylib emits into `ObjectAffects.location` alongside the legacy `HITROLL`/`DAMROLL`/`AC`. `equip_apply::map_legacy_apply_location` only knew the legacy aliases. Fixed inline (Wave 7) by adding pass-through arms for the modern names. Remaining unsupported locations after the fix: `AGE` / `CHAR_HEIGHT` / `CHAR_WEIGHT` / `COMPOSITION` (cosmetic, deliberately skipped — see the ObjectAffects mapping entry above).

## Resolved

### 2026-05-12 · Quest dialogue REGEX matcher (RESOLVED)

**Outcome:** `regex = "1"` added to `mud-server`'s `Cargo.toml`. The `"REGEX" =>` arm in `crate::quest_dialogue::matches` now compiles each keyword as a `(?i)` regex and runs `is_match` against the utterance. Invalid patterns log via `tracing::warn!` and fall through to a `CONTAINS` check on the same keyword — author error in a single keyword doesn't silently no-match the whole dialogue. No per-row cache yet; the per-utterance compile cost is in the noise next to the surrounding DB calls. Tests in `quest_dialogue::tests` cover a real `hello.*world` pattern + invalid-pattern fallback.

### 2026-05-12 · Quest variable Lua setvar / getvar bindings (RESOLVED)

**Outcome:** New `LuaQuest` userdata wraps `(character_id, quest_zone, quest_id)` and exposes `:getvar(name)` / `:setvar(name, value)` / `:clearvar(name)`. Reached from a trigger body via `actor:active_quest(quest_zone, quest_id)` — returns `nil` when the actor isn't a player (no `Account` component). The storage is a new `QuestVariableCache` resource on the world, mirroring the shape of `EntityVariableCache` (tombstones-until-flush, drain_dirty returning sets + clears). `quest_vars::quest_var_flush_tick` in mud-server drains every 10s and persists each dirty quest via `mud_db::quests::set_quest_variable`, keyed by the CharacterQuest row id (resolved per-flush via `find_character_quest`). Bulk hydration on boot was deliberately skipped — first-touch reads return nil, which matches the empty-row-from-DB shape, and the cache fills lazily on first `setvar`. Tests in `mud-script/src/lib.rs` cover round-trip / nil-clears / non-player-returns-nil / drain-payload.

### 2026-05-12 · Quest EVENT-trigger dispatcher (RESOLVED)

**Outcome:** `Events` table now has a runtime catalog. New `mud_db::events::list_all` returns `Vec<EventRow>` (id / name / active / start_date / end_date / recurring). The mud-server `events` module hydrates an `EventsCatalog` resource and runs an `events_poll_tick` every 60s (also on tick 0 so a freshly-active event fires once on boot). The drain (`drain_events_inbox`) reconciles each `(id, active)` against the catalog and fires `quest_triggers::dispatch_event_trigger(world, event_id)` on the off → on edge only — an event that stays on between polls doesn't re-fire, and on → off resets the gate so a subsequent re-activation fires again. Date-window interpretation is intentionally not done today; admins flip `Events.active` manually (via Muditor or admin tooling) and the runtime observes the flip on the next poll. The dispatch function's `#[allow(dead_code)]` is gone.

### 2026-05-12 · Wave 4.10 conditional rewards UX polish (RESOLVED)

**Outcome:** `cmd_qreward` was already routing through the synchronous Lua-eval gate; the polish pass tightened three UX nits:

1. **Empty-list message distinguishes "no completed quests" from "everything claimed"** — `qreward_list_all` now reads the per-character quest list and tailors the no-results line to whichever case applies, so a player who's burned through their pending claims sees "All your conditional / choice rewards from your completed quests have been claimed." instead of the generic empty-state line.
2. **Condition-not-met rejection echoes the actual expression** — `qreward_claim` now refuses with `"You don't currently meet that reward's condition: <expr>"` so the player (and any staffer they ask) can immediately see *why*. The legacy line was opaque when a re-classed player hit a class-locked condition.
3. **Successful-claim message tallies remaining pending claims** — after a claim writes the new `claimed_rewards` array, a fresh `count_pending_claims` walk reports "All your conditional / choice rewards are now claimed." (zero) or "N rewards still pending — type `qreward` to view." (one+).

Auto-grant path was already correct: unconditional non-choice rewards grant automatically on quest completion; conditional rewards go into the pending list. New test in `commands::quests::tests` (`eval_quest_availability_*`) locks in the condition-gate contract — literal true / false / arithmetic / fail-open-on-compile-error — so a refactor of the eval helper can't silently change the qreward refusal path's semantics.

A fully automatic "evaluate conditions at completion time" would still need the per-tick drain pattern hinted at in the original parking-lot note; that's a separate iteration if the UX feedback ever asks for it. Today's path is correct.

### 2026-05-12 · LoginRequests polling-resume + denial/expiry notice (RESOLVED — design replaced)

**Original parking entries:** Two follow-ups against Wave 6.4's
LoginRequests flow — (a) keep the session parked and resume inline
on APPROVE instead of forcing reconnect, (b) emit
`LOGIN_APPROVAL_DENIED` / `_EXPIRED` SystemText from the parked conn.

**Outcome:** the underlying design was wrong. The login-time approval
gate blocked play entirely on every login, including for established
characters whose names had been live for years. Replaced with a
per-character one-shot **name-approval** flow on the `Characters`
table:

- `Characters.name_approved Boolean @default(true)` column (existing
  characters auto-grandfathered).
- Runtime toggle moved from `security.login_approval_required` to
  `social.name_approval_required` (default OFF).
- `LoginRequests` table + `LoginRequestStatus` enum + four
  `LoginStage.LOGIN_APPROVAL_*` variants **dropped** from the schema.
- `NameApprovalPending` ECS marker attached at spawn iff column is
  `false`. Player can play normally (move / look / fight); every
  social command (`tell`, `reply`, `say`, `whisper`, `gsay`,
  `gossip`, `music`, `shout`, `qsay`, `ctell`, `invite`) refuses via
  `commands::name_approval_gate` until staff resolves.
- Resolution commands (Immortal+): `approve_name <character>` (keep
  name, flip column + drop live marker + DM player) and
  `reject_name <character> <new-name>` (force-rename + auto-approve;
  refuses while target is online to avoid Named-cache drift).
- Player self-diagnosis: `name_status`.
- Periodic `expire_old` tick removed; the per-row gate has no TTL.
- The retired commands (`approve_login`, `deny_login`, `lreqs`) and
  the `mud_db::login_requests` module are gone. The registry test
  in `commands::tests` asserts they don't resurface.

**Tests:** `character_name_approval_round_trip`,
`character_name_approval_defaults_true` (live-DB);
`name_approval_gate_blocks_when_marker_present`,
`name_approval_gate_clears_when_marker_removed` (unit).

**Muditor-side fallout:** the LoginRequests web-UI approval surface
(if any) is now stale — it queries a dropped table and never gets
hits. Not part of this refactor; safe to leave broken until the
Muditor side gets a dedicated pass.

### 2026-05-12 · ObjectAffects → ObjectEffects migration mapping (RESOLVED)

**Outcome:** mapping table approved and executed (Wave 3.4-3.7). The legacy `ObjectAffects` table is gone; all stat-modifier rows now live in `ObjectEffects` with `effect_id = 3` (the "modify" Effect) and `modifier_data = {"target": "<stat>", "amount": <int>}`. The runtime path is `mud_server::equip_apply::apply_object_to_wearer` → branch on `EffectDef.effect_type == "modify"` → `apply_modify_delta(target, amount)`.

**Counts:**
- 4083 ObjectAffects rows scanned
- 3743 ObjectEffects rows written (after per-(object, target) accumulation)
- 339 rows skipped:
  - 298 saving throws (SAVING_SPELL: 184, SAVING_BREATH: 70, SAVING_PARA: 38, SAVING_ROD: 3, SAVING_PETRI: 3) — saving-throw system redesigned, no modern target
  - 26 cosmetic (AGE: 16, CHAR_HEIGHT: 7, CHAR_WEIGHT: 3) — no consumer
  - 8 SIZE — no modify-target wired (and Size is a discrete enum, not numeric)
  - 3 COMPOSITION — enum dropped
  - 2 MANA — no mana stat in new combat
  - 1 GOLD — joke-item modifier, no apply path
  - 1 FOCUS with zero delta — no-op
- 0 errored

**Approved mapping** (locked at table top):
| Legacy `location` | New target | Scale |
|---|---|---|
| `HITROLL` | `accuracy` | × 2 |
| `DAMROLL` | `attack_power` | × 5 |
| `AC` | `armor_pct` | × −5 (legacy AC inverted) |
| `ACCURACY`, `ATTACK_POWER`, `SPELL_POWER`, `EVASION`, `HARDNESS`, `WARD_PCT`, `PEN_FLAT`, `PEN_PCT` | direct | × 1 |
| `ARMOR_RATING` | `armor_pct` | × 1 |
| `SOAK` | `armor_flat` | × 1 (rename) |
| `STR/DEX/CON/INT/WIS/CHA` | `<stat>_bonus` | × 1 |
| `HIT` / `MAX_HP` | `max_hp` | × 1 |
| `MOVE` / `MAX_MOVEMENT` | `stamina_max` | × 1 |
| `FOCUS`, `PERCEPTION`, `HIT_REGEN`, `HIDDENNESS` | direct (apply_modify_delta keys) | × 1 |

**Schema change required:** dropped the `@@unique([objectZoneId, objectId, effectId])` constraint on `ObjectEffects`. Multiple "modify" rows per object are legitimate (an item granting STR+2 AND DEX+3 is two rows).

**Code added:**
- `_bonus` aliases (`str_bonus`, `dex_bonus`, `con_bonus`, `int_bonus`, `wis_bonus`, `cha_bonus`) on `commands::apply_modify_delta`
- `stamina_max` alias alongside `max_move`/`max_stamina`
- `pen_flat` and `pen_pct` arms (previously absent)

See [migration-plan.md row 3.4-3.7](./migration-plan.md) and `fierylib/scripts/migrate_object_affects.py`.
