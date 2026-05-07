# fierymud-rs Code Review - 2026-05-07

Scope: Rust runtime crates in this repository. I did not compare against the
original C++ source line-by-line; parity notes below are inferred from this
codebase, existing backlog files, and the runtime design already present here.

Verification run:

- `cargo test --workspace --lib --bins` passed: 212 unit tests.
- `cargo clippy --workspace --all-targets -- -D warnings` passed.
- `set -a; source .env; set +a; cargo test -p mud-db --test integration -- --ignored --nocapture`
  passed: 9 live-DB integration tests.

DB/schema context:

- Runtime DB: `.env` points at local Postgres database `fierydev`.
- Schema source: `../muditor/packages/db/prisma/schema.prisma`, discovered via
  `../fierylib/.env`.
- Live DB snapshot reviewed: 121 public tables; largest imported tables include
  `RoomExit` (~25.9k rows), `Room` (~10.3k), `MobResets` (~5.6k), `Objects`
  (~3.5k), `Triggers` (~2.7k), and `Mobs` (~2.1k).

## Executive Summary

The codebase is in a strong place for a port: no obvious ad hoc SQL injection
patterns showed up, tests cover a good amount of command/combat/rendering
behavior, and the command registry has a clear role/permission model.

The highest priority problems are operational security, resource control, and a
few schema/runtime mismatches. Lua is not sandboxed, the admin HTTP API can run
unauthenticated if `ADMIN_TOKEN` is unset, login happens over plaintext TCP by
default, and several network/admin channels are unbounded. Those are
fix-before-public-hosting items.

The highest priority correctness bug I found is persistence around zero values:
`BankWealth` and drunkenness are saved only when nonzero, so clearing either to
zero can leave stale DB state.

The DB pass also found a latent destructive item persistence issue:
`CharacterItems` has fields for charges, condition, instance flags, custom
names, custom values, and liquid state, but the runtime deletes and reinserts
only prototype/location/container/equip fields. The current local DB has no
non-default instance state in `CharacterItems`, so this is not corrupting
existing local data today, but it will erase that state once those features are
used.

The deeper persistence audit found one documentation bug and several parity
gaps: the `save` command claims memorized spells persist even though the runtime
marks them session-only, active effects/cooldowns do not survive reconnect, and
pet/group/mount/guard state is currently session-scoped despite adjacent schema
support for some of it.

## High Priority Security Findings

### 1. Lua trigger execution is not sandboxed

Evidence:

- `crates/mud-script/src/lib.rs:105-114` creates `Lua::new()` with a TODO to
  remove `os` / `io` / `debug` modules later.
- `crates/mud-server/src/commands/admin_inspect.rs:273-286` gives Builder+
  users a `lua <code>` command with the same API surface as triggers.
- `crates/mud-server/src/admin.rs:607-652` exposes trigger fire/reload through
  the admin HTTP API.

Risk: if trigger rows or Builder accounts are compromised, Lua can likely reach
filesystem/process/debug functionality depending on what mlua exposes in this
configuration. Even without OS access, unbounded Lua can spin forever or allocate
heavily inside the single-threaded world tick.

Suggestions:

- Remove or replace `os`, `io`, `package`, `require`, `debug`, and dangerous
  standard globals immediately after creating the Lua state.
- Add an instruction-count or time-budget debug hook around trigger execution.
- Put a cap on yielded coroutine count and total wait backlog.
- Treat `Builder` access to `lua` as equivalent to shell-like power unless the
  sandbox is proven tight.

### 2. Admin HTTP auth is optional, but the API has full world-control powers

Evidence:

- `crates/mud-server/src/admin.rs:139-151` binds the admin server and sets
  `token` from optional `ADMIN_TOKEN`.
- `crates/mud-server/src/admin.rs:191-205` returns `Ok(())` when no token is set.
- `crates/mud-server/src/admin.rs:167-188` registers endpoints for command
  execution, teleport, spawn, pause, trigger reload/fire, and player mutation.

Risk: default bind is local-only, which is good, but a production deploy that
sets `ADMIN_LISTEN_ADDR=0.0.0.0:8080` without `ADMIN_TOKEN` exposes full game
control to the network.

Suggestions:

- Require `ADMIN_TOKEN` unless an explicit `ADMIN_ALLOW_UNAUTH_LOCAL=true` dev
  flag is set.
- Refuse to bind admin HTTP to a non-loopback address without auth.
- Use constant-time token comparison.
- Consider splitting read-only status endpoints from mutating endpoints.

### 3. Password login is plaintext by default and lacks auth-attempt throttling

Evidence:

- `crates/mud-server/src/main.rs:153-160` starts plaintext telnet on
  `0.0.0.0:4003` by default.
- `crates/mud-server/src/main.rs:164-197` makes TLS optional.
- `crates/mud-server/src/login.rs:870-875` verifies bcrypt per password attempt.
- `crates/mud-net/src/lib.rs:56-67` throttles connection rate, not failed login
  attempts.
- The schema already has `Users.failed_login_attempts`, `locked_until`, and
  `last_failed_login`; live `GameConfig` also has
  `security.max_login_attempts=3` and `security.login_timeout_minutes=15`, but
  the login path does not appear to enforce them.

Risk: passwords are exposed on the wire unless players use the TLS listener.
Separately, an attacker can keep a connection open and burn CPU through repeated
bcrypt checks.

Suggestions:

- Prefer TLS in production docs and server config; make plaintext opt-in for
  local/dev.
- Add per-IP and per-account failed-login throttles with exponential backoff.
- Add login-stage idle timeout.
- Consider telnet echo suppression for password prompts, or warn players that
  their client may locally echo the password.

### 4. Several network/control queues are unbounded

Evidence:

- `crates/mud-net/src/lib.rs:354` uses an unbounded outbound channel per
  connection.
- `crates/mud-server/src/main.rs:155` uses an unbounded inbound channel.
- `crates/mud-server/src/admin.rs:145` uses an unbounded admin command channel.
- `crates/mud-server/src/main.rs:214-218` uses an unbounded player update
  channel.
- `crates/mud-net/src/lib.rs:400-416` reads until newline without a maximum line
  length.

Risk: slow clients, long no-newline inputs, or admin/API floods can grow memory
without a firm cap.

Suggestions:

- Put a max command-line length in `mud-net` and disconnect on overflow.
- Use bounded channels for inbound/outbound/admin/player-update queues.
- Drop or disconnect clients whose outbound queue exceeds a threshold.
- Add body-size limits to axum admin routes.

## Correctness And Parity Issues

### 1. Zero bank balances may not persist

Evidence: `save_player` only writes `bank_wealth` when `bank != 0`
(`crates/mud-server/src/login.rs:1693-1702`).

Impact: if a character had money in the bank and withdraws to exactly zero, the
old nonzero DB value can survive reconnect.

Suggestion: always save `BankWealth`, or track the loaded original value and save
when changed.

### 2. Drunkenness clearing to zero may not persist

Evidence: `save_player` writes drunkenness only when `drunk > 0`
(`crates/mud-server/src/login.rs:1645-1656`).

Impact: a character who sobers up can reload with stale drunkenness.

Suggestion: mirror the bank fix: always save, or save when changed from the
loaded value.

### 3. `last_login` appears to mean "last save", not "last login"

Evidence:

- `save_state` sets `last_login = NOW()` on every save
  (`crates/mud-db/src/characters.rs:386-415`).
- autosave calls `save_all_online` every 3000 ticks
  (`crates/mud-server/src/main.rs:292-303`).

Impact: score/clientinfo may show the previous autosave/disconnect time rather
than the previous login time.

Suggestion: split this into `last_login_at` and `last_saved_at`, or update
`last_login` once at successful login instead of in generic state saves.

### 4. Production boots still seed test entities

Evidence:

- `crates/mud-server/src/main.rs:143-144` always calls `seed_test_mobs` and
  `seed_test_items`.
- `crates/mud-server/src/combat.rs:19-90` spawns a training dummy, rusty sword,
  and potion in The Void.

Impact: useful during development, but surprising in production and potentially
non-parity with imported reset content.

Suggestion: gate this behind `MUD_SEED_TEST_CONTENT=true` or move it to tests /
dev harness setup.

### 5. `wimpy_threshold` semantics look inconsistent

Evidence:

- Login hydration says absence means default flee behavior
  (`crates/mud-server/src/login.rs:1175-1181`).
- Save-side comment says absence means no auto-flee
  (`crates/mud-server/src/login.rs:1550-1554`).

Impact: players may be unable to distinguish "disabled" from "use default" in a
way that persists predictably.

Suggestion: model this explicitly as `Option<i32>` in runtime semantics:
`None = default`, `Some(0) = disabled`, `Some(n) = threshold`, or choose a single
meaning and align comments/UI.

## DB And Schema Findings

### 1. `CharacterItems` save is destructive for instance state

Evidence:

- Schema fields exist for `condition`, `charges`, `instance_flags`,
  `custom_name`, `custom_examine_description`, `custom_values`, `liquid_type`,
  `liquid_remaining`, `liquid_effects`, and `liquid_identified`
  (`../muditor/packages/db/prisma/schema.prisma:513-537`).
- Runtime load/save only covers `id`, `character_id`, `object_zone_id`,
  `object_id`, `container_id`, and `equipped_location`
  (`crates/mud-db/src/character_items.rs:18-31`, `103-165`).
- `save_for` deletes all rows for the character, then reinserts only those
  narrow fields.

Impact: item charges, wear/condition, custom names/descriptions, liquid
contents, instance flags, and arbitrary per-item state are lost on disconnect or
autosave once any feature starts using them.

Current DB state: 82 character item rows; none currently have non-default
charges, liquid state, or custom state. This is latent in the local data, but it
is a high-priority persistence contract issue.

Suggestion: either make runtime item components own the full instance state and
round-trip it, or change save to preserve untouched DB fields for existing item
rows. The delete/reinsert strategy is simple but incompatible with instance
state.

### 2. Character position/ghost persistence is declared in schema but not wired

Evidence:

- Schema comment says `Characters.position` is persisted so ghost state survives
  logout (`../muditor/packages/db/prisma/schema.prisma:620-621`).
- Runtime hydration inserts `Posture(Standing)` from gameplay state, but I did
  not find load/save logic for `Characters.position`.

Impact: current posture and death/ghost state can reset on reconnect. That
matters for death recovery, combat logout edge cases, and parity with the schema
contract.

Current DB state: all 5 local characters are `STANDING`, so this is not showing
as live data loss in the current fixture.

Suggestion: decide the canonical mapping between runtime `Posture`/`Ghost` and
the DB `Position` enum, then persist it alongside room, HP, and other character
state.

### 3. `GameConfig` is loaded, but many operational rows are ignored

Evidence:

- Loader reads all `GameConfig` rows into `RuntimeConfig`
  (`crates/mud-world/src/loader.rs:635-685`).
- Live DB has 55 config rows, including server, security, character, display,
  combat, progression, and timing values.
- Several important rows do not appear to drive their corresponding runtime
  behavior: `server.port`, `server.max_connections`,
  `server.max_command_queue_size`, `server.target_tps`, `server.tls_port`,
  `security.enable_tls`, `security.enable_new_player_creation`,
  `security.enable_debug_commands`, `security.max_login_attempts`,
  `security.login_timeout_minutes`, `display.default_starting_room`, and the
  character creation starting-stat rows.
- Runtime default telnet bind is `0.0.0.0:4003`, while live `GameConfig` has
  `server.port=4000`; runtime fallback starting room paths still fall back to
  `(0,0)`, while live `GameConfig` has `display.default_starting_room=3001`.

Impact: operators/editors can change rows that look authoritative but have no
effect. That is especially risky for security rows because a deployer may
believe throttling, TLS, creation gating, or debug-command gating is active when
it is not.

Suggestion: split config rows into "wired" and "reserved" groups, expose that in
admin docs, and wire security/server/start-room rows before treating the config
table as an operational control plane.

### 4. Live trigger data still contains legacy `.vnum` references

Evidence: 37 live `Triggers` rows still contain `.vnum` references. The runtime
removed synthetic `vnum` access in favor of composite `(zone, id)` lookups, so
these scripts are likely to error when their paths execute. The script error
logging path exists and currently has 4 rows in `script_error_log`, which is
good for detection but does not fix the content.

Impact: quest hand-ins, special room/object behavior, and converted DG scripts
can fail only when a player reaches the affected content, making this a
playtest-time parity trap.

Suggestion: run a focused trigger migration/audit over live DB scripts, not only
the checked-in Lua corpus. Add a CI or importer check that rejects new `.vnum`
references unless they are in comments or explicit compatibility helpers.

### 5. Quest schema/runtime exists, but live quest data is empty

Evidence: the schema has `Quest`, `QuestPhase`, `QuestObjective`,
`QuestReward`, and `CharacterQuest` tables, and the Rust DB layer has objective
progress helpers. In the reviewed local DB, `Quest` and `CharacterQuest` both
have 0 rows.

Impact: quest commands and objective progress logic are data-blocked. From a
player perspective, this leaves the world dependent on trigger-script quest
behavior and external knowledge rather than a visible journal/progress loop.

Suggestion: treat quest data import as a product milestone, not just a DB
milestone. Once even a small starter quest set exists, add login/newbie hints
that point players to `quests` and journal-style next steps.

### 6. Ability data is mostly cleaner, but a few content issues remain

Findings:

- Ability/area targeting consistency looked good in the DB pass: no obvious
  `target_scope` vs `is_area` mismatches.
- Room exits and reset references checked cleanly: no missing reset room/mob/object
  refs and no missing exit destination/key refs in the sampled integrity
  queries.
- `BANDAGE` still heals with formula `skill / 5`, which is 0 at skill 0, and it
  still does not appear to cleanse bleed.
- `TRIP_UP` has knockdown and damage effects, but still does not appear to set
  the target posture to resting/downed as earlier notes suggested.
- `EYE_GOUGE`, `REND`, and `ROAR` now have clearer scaling/status data, so the
  older stale concerns for those specific skills appear fixed.
- 25 abilities have no `AbilityMessages` rows. Many look passive/innate, so this
  may be fine, but active commands should not resolve with generic or missing
  combat text.

Suggestion: add a lightweight data audit test that checks active abilities have
messages, nonzero low-skill outcomes where intended, and explicit target/posture
effects for control skills.

### 7. Local character fixture data still has class gaps

Evidence: 4 of 5 local characters have no `class_id` (`BuilderChar`,
`TestMage`, `TestRogue`, `TestWarrior`). This may only be stale local fixture
data, but it contradicts the direction implied by the richer character-class
schema.

Impact: tests and manual play can accidentally validate fallback paths instead
of the class-backed paths real players should use.

Suggestion: rerun or repair the seed/migration for local development characters,
then add a fixture check for expected class assignments.

## Persistence Parity Audit

### What currently round-trips well

- Core character state: HP, stamina, location, recall, player flags, prompt,
  title, description, wealth, XP, skill points, hunger, thirst, staff invis,
  freeze lock, wimpy threshold, poof messages, core stats, and time played.
- Known abilities and proficiency via `CharacterAbilities`.
- Aliases via `CharacterAliases`.
- Script variables and trophy/kill-tracking JSON on `Characters`.
- Achievement unlocks and zone-visit progress through `character_achievement`.
  The live DB has 9 character achievement rows.
- Recent tell history is persisted to `tell_message`, though the live DB had no
  rows at review time.
- Mail and board posts use durable DB tables rather than `save_player`.
- Weather, clock, and corpses have shutdown snapshots; corpse snapshots are
  useful but intentionally lossy.

### 1. `save_player` is not atomic across the character's persisted state

Evidence: `save_player` updates the `Characters` row, then saves inventory,
drunkenness, script vars, trophy data, bank wealth, time played, abilities,
aliases, and core stats as separate async DB operations
(`crates/mud-server/src/login.rs:1507-1778`).

Impact: a partial DB failure can leave a mixed checkpoint. For example, location
and wealth can be saved while inventory fails, or abilities can fail and skip
later alias/core-stat persistence. This is most risky around inventory-moving
commands, bank/wealth changes, and manual `save` before dangerous content.

Suggestion: group the player checkpoint into one DB transaction where possible,
or write a `character_save_result` audit row that records which sub-saves
succeeded. At minimum, make the `save` command report partial failures instead
of always saying `Saved.`

### 2. Active effects do not persist despite a `CharacterEffects` schema

Evidence:

- Schema has `CharacterEffects` for runtime effects with duration, strength,
  source, and expiration (`../muditor/packages/db/prisma/schema.prisma:442-460`).
- Runtime effects live as ECS `EffectInstance` entities with `AppliedTo` and
  optional `ModifyDelta` (`crates/mud-world/src/components.rs:1372-1415`).
- I did not find load/save logic for `CharacterEffects`; live table count is 0.

Impact: reconnect clears buffs, debuffs, bleed, blind, stun-backed status,
modify-stat effects, room effects, and admin-applied effects. This can be a
player-friendly reset for some temporary effects, but it also lets players clear
negative effects by reconnecting and loses positive long-duration buffs after a
network drop.

Suggestion: classify effects by persistence policy: `SESSION_ONLY`,
`SAVE_REMAINING_DURATION`, or `WALL_CLOCK_EXPIRES_AT`. Then wire
`CharacterEffects` only for the effects that should survive reconnect.

### 3. Memorized spells are session-only, but `save` says they persist

Evidence:

- `MemorizedSpells` explicitly says it is session-only and not persisted
  (`crates/mud-world/src/components.rs:1089-1094`).
- The `save` help says it persists "memorized spells back to the schema"
  (`crates/mud-server/src/commands/save.rs:24-27`).

Impact: players who manually `save` before logging out can reasonably expect
prepared spells to survive, then lose them on reconnect. That is a trust issue
even if session-only memorization is the intended v1 design.

Suggestion: either add a small persisted spell-prep table, or change `save` and
`memorize` wording to clearly say prepared spells last only for the current
session.

### 4. Cooldowns reset on reconnect

Evidence: cooldowns are stored as `Cooldowns { ready_at: HashMap<i32, Instant> }`
(`crates/mud-world/src/components.rs:884-887`) and written after successful
ability use (`crates/mud-server/src/commands.rs:9780-9794`). There is no DB
round-trip path.

Impact: reconnecting clears ability cooldowns. The live DB currently has only
short cooldowns (`Disengage` 12s, `Trip Up` 8s, `Backstab`/`Buck` 6s), so this
is low severity today. It becomes an exploit as soon as longer class, quest, or
consumable cooldowns are added.

Suggestion: keep short combat cooldowns session-only if desired, but persist any
cooldown above a threshold as wall-clock `ready_at`.

### 5. Inventory-like storage loses per-instance state in several paths

Evidence:

- Carried inventory through `CharacterItems` loses charges, condition, custom
  values, liquid state, and similar instance fields as described above.
- Corpse shutdown snapshots store only item prototype keys and are explicitly
  lossy for charges, light fuel, nested container contents, and other instance
  state (`crates/mud-server/src/corpses.rs:1-13`, `24-47`, `72-86`).
- Corpse snapshots also run only on graceful shutdown; an ungraceful crash
  between death and looting can still lose corpse contents.
- House items are persisted by prototype only. `PlayerHouseItem` has
  `condition` and `custom_values`, but `place_item` inserts only room/prototype
  (`crates/mud-db/src/housing.rs:178-199`), and the command path moves the item
  in-memory before the fire-and-forget insert completes
  (`crates/mud-server/src/commands/info.rs:10187-10225`).

Impact: object state is inconsistent depending on where the object is: carried,
in a corpse, placed in a house, or still in the world. Charges can refill, liquid
containers can reset, custom state can disappear, and a failed async house insert
can leave the item moved out of inventory without a durable row.

Suggestion: define one reusable `ItemInstanceState` payload and use it for
`CharacterItems`, corpse snapshots, house items, mail/account storage, and any
future auction/trade storage. Commands that move items into durable storage
should confirm the DB write before removing the item from the player's durable
inventory state.

### 6. Pets, followers, groups, mounts, and guarding are session-scoped

Evidence:

- Runtime relationship state uses `Follower`, `Mounted`, `RiddenBy`, and
  `Guarding` entity links (`crates/mud-world/src/components.rs:892-901`,
  `802-809`).
- Hiring a pet spawns a mob with `Follower(player)` in-memory
  (`crates/mud-server/src/commands/info.rs:3479-3507`).
- Schema has `CharacterPets`, but I did not find runtime load/save for it; live
  table count is 0.

Impact: group/follow/guard/mount links disappearing on reconnect may be
acceptable. Paid or summoned pets disappearing is more visible, especially if
players spend gold or quest resources to obtain them.

Suggestion: document which relationship states are intentionally session-only.
If pets are meant to be durable, wire `CharacterPets` for hired/charmed/summoned
companions that should survive logout, with clear rules for temporary summons.

### 7. Ignore lists are session-only

Evidence: `IgnoreList` is a runtime component only
(`crates/mud-world/src/components.rs:922-953`), and `ignore` / `unignore` mutate
that component (`crates/mud-server/src/commands/tells.rs:247-307`). The user
schema has a `preferences` JSON column, but this list is not loaded from or
saved into it.

Impact: players often expect ignore/mute choices to persist. Session-only
ignore means a harassing player can resume contact after the target reconnects.

Suggestion: persist ignores either in `Users.preferences` or a normalized
account/character ignore table. Keep `LastTeller` session-only; that is fine.

## Code Health Notes

- SQL access looked consistently parameterized through `sqlx`; I did not see
  obvious string-built SQL queries in the reviewed paths.
- Workspace-level `unsafe_code = "forbid"` is good. The one exception is
  `mud-script`, which allows unsafe for the raw `World` pointer bridge. That
  bridge deserves periodic focused review because it is central to Lua callback
  soundness.
- The command registry's startup validation and role checks are solid. The
  tests include a player-role refusal case for admin commands.
- The renderer has thoughtful XML-lite handling and tests around prompt/color
  edge cases. Keep adding tests whenever authored content introduces new tag
  shapes.
- Async fire-and-forget DB writes appear in several command areas. They are fine
  for feedback/audit-style paths, but anything that affects player progress
  should return success/failure to the player or route through a durable queue.

## Player Experience Review

### What is already working well

- The terminal UI has a consistent color vocabulary: cyan headers, yellow
  highlights, red danger, dim metadata.
- Help is categorized and command registration forces help summaries to exist.
- Score, identify, compare, spell listings, prompt rendering, and combat damage
  have clearly received polish passes.
- Colorblind/plain-text mode exists via `COLOR_BLIND`.

### Highest-impact UX improvements

1. Add a first-login "what now" path.

   After character creation and room entry, show a short actionable line such as:
   `Try: look, exits, score, inventory, help newbie.`

   Also consider a small `newbie` or `tutorial` command that lists the next 3-5
   practical goals: find a trainer, practice a skill, equip a weapon, kill a
   safe target, recall.

2. Add danger readability before combat.

   Players need an at-a-glance way to avoid the current "mid-level player gets
   deleted by an unexpected town mob" problem already noted in
   `PLAYTEST_NOTES.md`. A `consider <target>` command already exists; polish it
   into the first-login/help flow and make sure the color bands are obvious:
   harmless, risky, deadly, suicidal. Also color mob level/difficulty in
   `examine`.

3. Make exits and interactables more scannable.

   In room look output, color exits by state:
   open/obvious = cyan, closed = yellow, locked = red/dim if known, hidden =
   omitted unless discovered or staff. For objects, consider a subtle section
   split for "Obvious objects" vs "Items on the ground" vs "People here".

4. Improve ordinary failure phrasing.

   Keep "You can't do that" for permission-hiding/admin cases, but use specific
   advice for normal play:

   - `You need a free hand to wield that.`
   - `That spell needs a target. Try: cast 'magic missile' <target>.`
   - `You are carrying too much. Drop something or raise Strength.`

5. Give death and corpse recovery more guidance.

   On death/release, include the corpse location and recovery hint. If a mob can
   pick up corpses, tell the player what happened in clear terms and leave a
   trail via `corpse` / `recover` / staff-visible diagnostics.

6. Use more color, but keep it semantic.

   More color is useful for:

   - HP/stamina thresholds.
   - exits and door states.
   - hostile vs neutral vs friendly actors.
   - quest availability/completion.
   - item rarity or magical status after identify.

   Avoid coloring every noun. The current restrained palette is a strength; add
   color where it changes player decisions.

7. Add a journal/quest breadcrumb system.

   Quest API/data is still a known gap because the live quest tables are empty.
   When quest data lands, player flow will benefit from `quests`, `quest <name>`,
   and `where next` style hints. MUDs can be opaque; a concise journal prevents
   players from needing external notes for basic progression.

8. Make account/character creation less raw.

   The flow works, but the final welcome currently includes internal character
   and user IDs (`crates/mud-server/src/login.rs:859-867`). That is useful for
   testing but not player-facing. Move IDs behind Builder+ diagnostics or
   `clientinfo`.

## Suggested Ticket Order

1. Require or explicitly gate unauthenticated admin HTTP.
2. Sandbox Lua and add an execution budget.
3. Add max line length, bounded queues, and auth-attempt throttling.
4. Preserve full `CharacterItems` instance state on save.
5. Make player checkpointing transactional or report partial save failures.
6. Fix zero-value persistence for bank wealth and drunkenness.
7. Wire security-critical `GameConfig`/`Users` auth lockout fields.
8. Migrate live trigger `.vnum` references.
9. Decide persistence policy for active effects, memorized spells, cooldowns,
   pets, and ignores.
10. Gate test seed content behind a dev flag.
11. Split `last_login` from save timestamp and persist character position/ghost
   state.
12. Add first-login guidance and surface `consider <target>`.
13. Improve ordinary failure messages and death recovery hints.
