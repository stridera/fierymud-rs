# Command Parity: legacy → fierymud-rs

**Generated:** 2026-05-05
**Sources:** `fierymud_legacy/src/interpreter.cpp` (lines 354–996, `cmd_info[]`) and `fierymud-rs/crates/mud-server/src/commands/*.rs` (`inventory::submit!` blocks).
**Scope:** Excludes OLC (`*edit`, `*copy`, `olc`, etc. — handled by muditor) and the DG scripting subsystem (`m*` mob-script verbs, `attach`/`detach`, `tlist`/`tnum`/`tsearch` raw-DG commands — replaced by Lua, but see "Trigger / Lua debugging" section below).

## Summary
- Total legacy commands surveyed (after OLC + DG exclusions): **~290**
- Implemented in fierymud-rs (with at least one alias matching): **~155**
- Missing in modern: **~135**
- Modern-only additions: **~20**

Counts are approximate because many legacy entries are pure aliases (`go` → `do_move`, `'` → `do_say`) and some socials collapse onto `do_action`. Socials are tracked in bulk below.

## Caveats
- Legacy aliases multiple keywords to one handler (`hold`/`grab`, `north`/`n`, `tell`/`'`). They're collapsed in this report.
- A single legacy entry with multiple `subcmd` modes (e.g., `do_csearch` services `clist`/`cnum`/`csearch`) is listed once if all three modes have the same intent.
- "Implemented" means a name overlap exists. Behaviour parity isn't audited here.
- A legacy command routed to `do_not_here` is a stub that fires only inside specific shop/bank rooms — those are still listed because they need a Rust handler in those rooms.

---

## Implemented (alphabetical)

| Legacy | Rust file | Notes |
|---|---|---|
| abandon | quests.rs | quest abandon |
| abort | spells.rs | abort cast |
| accept | info.rs (`accept`) | group accept |
| afk | info.rs | toggle |
| alias / unalias | info.rs | |
| ask | room_chat.rs | |
| assist | combat.rs | |
| at | admin_world.rs (`force` covers? — TBD) | partial |
| backstab / bs | combat.rs | |
| balance / bal | balance.rs | bank balance |
| ban | admin_management.rs | |
| bandage | combat.rs | |
| bash / bodyslam / maul | combat.rs | merged |
| berserk | combat.rs | |
| boards / board / post / delpost / editpost | boards.rs | |
| breathe | combat.rs | |
| brief / compact / norepeat | info.rs (toggles) | |
| bribe | info.rs | |
| buck | combat.rs | |
| bug / idea / typo / petition | feedback.rs | |
| buy / sell / list / value / appraise | info.rs (shop) | |
| cancel | spells.rs | |
| cast / c | spells.rs | |
| chant | spells.rs | |
| clan | clan_chat.rs | |
| close / open / lock / unlock | info.rs | doors |
| color / colour | info.rs | |
| commands | info.rs | |
| compare | info.rs | |
| conceal | combat.rs | |
| consider / con | combat.rs | |
| consent | info.rs | |
| corner | combat.rs | |
| credits / motd / news / policies / rules | info.rs | textviews |
| ctell / ct | clan_chat.rs | |
| date / time / uptime | info.rs | |
| deaf / notell | info.rs | toggles |
| decline | info.rs | group decline |
| deposit / withdraw | info.rs (bank, alias of `do_not_here`) | |
| description / desc | info.rs | |
| diagnose / glance | info.rs | |
| disarm | combat.rs | |
| disband / dismiss | info.rs | group |
| disengage | combat.rs | |
| dismount / mount | info.rs | |
| doorbash | combat.rs | |
| drag | combat.rs | |
| drink / sip / taste / quaff | info.rs / spells.rs | |
| drop / junk / trash / get / take / put | info.rs | inventory |
| eat | info.rs | |
| effects / affects / aff | info.rs | |
| emote / : | room_chat.rs | |
| enter | enter.rs | |
| equipment / eq | info.rs | |
| examine / exam / exa | info.rs | |
| exits / ex | info.rs | |
| experience / exp / xp | info.rs | |
| extinguish / douse | info.rs | |
| fill / pour | info.rs | |
| firstaid | combat.rs | |
| flags | info.rs | |
| flee | combat.rs | |
| fly / walk / land | info.rs | |
| follow / shadow / unfollow | info.rs | |
| force | admin_world.rs | |
| forget | spells.rs | |
| freeze | admin_world.rs | |
| give | info.rs | |
| gossip / / | channels.rs | |
| goto | admin_world.rs | |
| gouge | combat.rs | |
| group | info.rs | |
| gsay / gtell / gecho / gt | room_chat.rs | |
| guard | combat.rs | |
| help | info.rs | |
| hide | info.rs | |
| hire | info.rs | |
| hit / kill / k / attack / murder | combat.rs | merged |
| hitall / tantrum | combat.rs | merged |
| hold / grab / wield / wear / remove / rem | info.rs | equip |
| house / visit | info.rs / housing.rs | |
| identify / id | info.rs | |
| ignore / unignore | tells.rs | |
| innate | quests.rs | |
| insult | room_chat.rs | |
| inventory / i / inv | info.rs | |
| invite | info.rs | group |
| kick | combat.rs | |
| lasttells / lt | tells.rs | |
| layhands / lay | combat.rs | |
| level | info.rs | |
| light | info.rs | |
| load / loadobj / loado | admin_world.rs | |
| look / l | info.rs | |
| lure | combat.rs | |
| mail / mailbox / readmail / delmail | mail.rs | |
| memorize / mem / pray / study | spells.rs | |
| music | channels.rs | |
| order | info.rs | group |
| perform | spells.rs | |
| pick | spells.rs | doorpick AND lockpick — verify which |
| practice / prac | info.rs | |
| prompt / display | info.rs | |
| purge | admin_world.rs | |
| quaff | info.rs | |
| quest / qstat / qlist / quests | quests.rs | |
| quit / qu | info.rs | |
| read | info.rs | |
| recall / home / rec | recall.rs | |
| recite | info.rs | |
| recline / rest | info.rs | |
| release | release.rs | |
| rend | combat.rs | |
| reply / r | tells.rs | |
| report / rep | status_lists.rs | |
| rescue / res | combat.rs | |
| restore | admin_world.rs | |
| retreat | combat.rs | |
| rstat / mstat / ostat / sstat / tstat / astat / zstat / stat | admin_inspect.rs | |
| roar / howl | combat.rs | merged |
| roundhouse | combat.rs | |
| save | save.rs | |
| say / ' | room_chat.rs | |
| scan | info.rs | |
| score / sc | info.rs | |
| scripterrors / scripterr | admin_inspect.rs | new — replaces `do_mob_log` etc. |
| search | info.rs | |
| set | admin_inspect.rs | wizard set |
| setrecall | setrecall.rs | |
| shout | channels.rs | |
| show | admin_inspect.rs | |
| sit / stand / sleep / wake / kneel | info.rs | postures |
| skills / spells / abilities / abil / songs / chants | info.rs | |
| slots | info.rs | |
| sneak | combat.rs | |
| socials | status_lists.rs | |
| split | info.rs | |
| springleap | combat.rs | |
| stomp | combat.rs | |
| style | info.rs | |
| summon | admin_world.rs | (admin summon) |
| sweep | combat.rs | |
| syslog | admin_inspect.rs | |
| tame | combat.rs | |
| tell / t | tells.rs | |
| teleport | admin_world.rs | |
| throatcut | combat.rs | |
| title | info.rs | |
| toggle | info.rs | |
| touch (object) | setrecall.rs | |
| track / hunt | info.rs | |
| train | info.rs | |
| transfer | admin_world.rs | |
| triggers / trigs | admin_inspect.rs | new — see Trigger debugging below |
| firetrig | admin_inspect.rs | new |
| tripup / trip | combat.rs | |
| unban | unban.rs | |
| visible / vis | info.rs | |
| wave / tap | info.rs | object signals |
| weather | info.rs | |
| where | admin_world.rs | |
| who | info.rs | |
| whisper | room_chat.rs | |
| whoami | info.rs | |
| wimpy / wi | info.rs | |
| wiznet / ; | channels.rs | |
| world / users / stats | info.rs | merged |
| version | info.rs | |
| auto* (autoexit/autoloot/autogold/autoassist/autosplit) | info.rs | toggles |
| **Movement directions:** north/n, south/s, east/e, west/w, up/u, down/d, ne, nw, se, sw, in, out | movement_directions.rs | |

---

## Missing in modern (categorized by importance)

### Player essentials (port these for player-facing parity)

| Legacy | Purpose | Notes |
|---|---|---|
| `alert` / `meditate` | Combat-stance toggles (alert ↔ resting transitions and meditation focus state). | Underpins spell prep and ambush mechanics. |
| `bite` (combat-bite — different from social) | Vampire/animal bite attack — note: legacy table has `bite` as `do_action` only, so this is a true social, see flavor list. | — |
| `camp` | Out-of-room rest / log-out-safe state (`do_camp`). | Frequently used by players. |
| `claw` | Druid/animal claw attack (`do_claw`). | |
| `cls` / `clear` | Clear screen / ANSI reset. | Trivial. |
| `electrify` | Class skill (`do_electrify`). | |
| `feel` / `hunger` / `thirst` reporting | Multiple are socials, but `experience` already covers some. | Confirm which legacy info commands aren't covered. |
| `first aid` (two-word) | Two-word command form — Rust has `firstaid` (joined) but not the spaced legacy variant. | Minor; aliasing may be enough. |
| `gretreat` | Group retreat — coordinated group escape. | Player essential for group play; `retreat` is solo. |
| `greport` | Group report (broadcasts hp/mv to group). | Common in group play. |
| `last gossips` / `lastgos` | Replay of recent gossip channel messages. | Players use this constantly. |
| `leave` | Exit a vehicle / object / boat. | |
| `levelup` (no separate — `level` shows info; legacy has it merged). | n/a | already covered |
| `palm` | Thief skill — palm an item. | |
| `peck` | Avariel/avian skill (`do_peck`). | |
| `point` | Point at someone/something (visible gesture). | |
| `qsay` / `qecho` | Quest channel — for active-quest players. | Quest UX. |
| `ptell` (player-tells admin? — actually `do_ptell` is god-only; skip from "player essentials"). | — | move to admin |
| `scribe` | Scribe spell into spellbook (mage class). | Class essential. |
| `shapechange` | Druid/shifter form change. | Class essential if those classes are in. |
| `steal` | Thief skill. | |
| `stow` | Hide item on person / palm-into-inventory. | |
| `subclass` | Class advancement / subclass selection. | |
| `summon (mount)` | The legacy `do_summon_mount` — Rust has `summon` for admin transfer; player-facing summon-mount is missing. | |
| `trophy` | Show kill list / xp trophies. | Player-facing info. |
| `aggr` | Show what's currently aggro on you (`do_aggr`). | Useful in combat. |

### Builder / admin essentials

| Legacy | Purpose | Notes |
|---|---|---|
| `advance` | Set a player's level (admin level-up). | |
| `autoboot` | Schedule / view server reboot. | |
| `boardadmin` | Admin tools for board content. | |
| `coredump` | Force coredump for debugging. | Head-coder only. |
| `dc` | Disconnect a player by descriptor. | |
| `echo` | God-mode echo to a player or room. | |
| `gecho` | Global echo. | |
| `qecho` | Quest-channel echo. | |
| `grant` / `revoke` / `ungrant` | Permission grants on commands (the `do_grant` family). | Important for role-based access. |
| `hcontrol` | House control (admin: assign houses to players). | |
| `hhroom` | Clone a room as a house room (`do_rclone`). | Builder-flow. |
| `hotboot` | Live restart preserving connections. | Operations-essential. |
| `inctime` / `hour` | Force-advance game time. | Useful for testing day/night triggers. |
| `infodump` | Dump live world state to disk. | |
| `invis` | Set immortal invis level. | Used constantly by gods. |
| `ispell` | Inline spell-check tool? — confirm. | |
| `last` | Show last-login info for a player. | |
| `linkload` | Load+link a mob/object into the world. | Builder testing. |
| `listspells` | List all spells (admin). | |
| `mute` (`do_wizutil` SCMD_SQUELCH) | Silence a player. | |
| `naccept` / `ndecline` / `nlist` | Approve/deny new character names. | Admin moderation. |
| `note` | Imm note feature. | |
| `notitle` | Strip a player's title. | |
| `objupdate` | Refresh/update objects from prototype. | Builder maintenance. |
| `page` | Send pager-style admin tell. | |
| `pain` / `rpain` | Inflict damage (`do_pain`/`do_rpain`). | Admin testing. |
| `pardon` | Clear a kill flag / arrest record. | |
| `peace` | Stop all combat in a room. | Common god command. |
| `pfilemaint` | Pfile maintenance. | |
| `players` | List all players (offline + online stats). | |
| `poofin` / `poofout` | Set immortal arrival/departure messages. | |
| `ptell` | God-only player tell to multiple players. | |
| `rrestore` | Restore a whole room (`do_rrestore`). | |
| `rename` | Rename a character (admin). | |
| `reroll` | Reroll a player's stats. | |
| `send` | Send raw text to a descriptor. | |
| `skillset` | Set a player's skill levels. | Builder/test essential. |
| `snoop` | Spy on another connection. | Critical admin tool. |
| `shutdown` / `shutdow` / `reload` / `terminate` | Server-control commands. | Ops-essential; modern probably uses systemd but still need in-game variants. |
| `switch` | Possess a mob. | Common builder/QA tool. |
| `thaw` | Unfreeze a frozen player. | |
| `unaffect` | Strip a player's affects. | |
| `users` | List connections (Rust has `users` aliased to `world` info, but legacy `users` is admin-detailed — **verify**). | |
| `varset` / `varunset` | Set DG/Lua variables on entities. | **Critical for Lua debugging.** See trigger section. |
| `viewdam` | View damage tables (combat damage debugger). | Builder QA. |
| `wizlock` | Block non-admin logins. | |
| `xnames` | Manage banned-name list. | |
| `zreset` | Reset a zone (re-run zone-reset). | Builder critical. |
| `zsearch` / `zlist` / `znum` | Search/list zones. | |
| `vlist` / `vnum` / `vsearch` / `vstat` (the bare/multi-type forms — Rust has per-type osearch/msearch/rsearch). | Bulk vlist still missing. | |
| `vitem` / `vwear` | View item / wearable properties (`do_vitem`/`do_vwear`). | Builder QA. |
| `estat` / `restat` / `oestat` | Extra-description stats (legacy `do_estat`). | Editing aid; possibly muditor-only. |
| `pscan` | Scan all players in zone (`do_pscan`). | |
| `clist` / `csearch` (classes), `elist` / `esearch` (extras), `ksearch`, `ssearch`/`slist`/`snum` (skills/spells) | Builder catalog browsers. | Some may be muditor-only territory. |

### Nice-to-have flavor (mostly socials)

The legacy `do_action` table has **~135 social-only verbs**. None of them are wired in fierymud-rs as discrete commands; they appear to be served by a separate `socials` system. Inspect:

`ack, accuse, agree, amaze, apologize, applaud, ayt, beckon, beer, beg, bird, blink, bleed, blush, boggle, bonk, bored, bounce, bow, brb, burp, bye, cackle, chuckle, cheer, choke, clap, comb, comfort, cough, cringe, cry, cuddle, curse, curtsey, dance, daydream, dream, drool, duck, duh, embrace, envy, eyebrow, fart, flanic, flex, flip, flirt, fool, fondle, french, frown, fume, gag, gape, gasp, giggle, glare, glomp, glower, greet, grin, groan, grope, grovel, growl, grumble, halo, hi5, hiccup, hiss, hop, hug, hunger, imitate, impale, kiss, lag, laugh, lean, lick, love, massage, moan, moon, mosh, mourn, mumble, mutter, nap, nibble, nod, nog, noogie, nudge, nuzzle, panic, pant, pat, peer, pet, poke, ponder, pounce, pout, protect, puke, punch, purr, raise, ready, rofl, roll, ruffle, salute, scare, scold, scratch, scream, screw, seduce, shake, shiver, shrug, shudder, sigh, sing, slap, slobber, smell, smile, smirk, smoke, snap, snarl, sneeze, sniff, snicker, snoogie, snore, snort, snowball, snuggle, spam, spank, spit, squeeze, stare, steam, stroke, strut, sulk, swat, sweat, tackle, tango, taunt, tarzan, tease, thank, think, throw, tickle, tip, tongue, tug, twibble, twiddle, twitch, veto, wait, wet, whap, whatever, whine, whistle, wiggle, wince, wink, worship, yawn, yodel, zone`

These are the legacy `socials.dat` content. Audit whether `socials` registry in `mud-server/src/commands/status_lists.rs` actually loads all of them — if so, this section is fully covered; if not, that loader is the gap.

### Probably skip

| Legacy | Reason |
|---|---|
| `attach`, `detach`, `tlist`, `tnum`, `tsearch` (raw-DG) | DG scripting subsystem; replaced by Lua. |
| `tedit`, `trigedit`, `trigcopy` | OLC. |
| `dig` | OLC quick-build — handled by muditor. |
| `aedit`, `gedit`, `hedit`, `iedit`, `medit`, `mcopy`, `oedit`, `ocopy`, `redit`, `rcopy`, `sedit`, `sdedit`, `zedit` | OLC. |
| `qadd`, `qdel` (legacy DG-quest) | Replaced by `qload`/`qgive`/`qcomplete` already in modern. |
| `m_run_room_trig`, `mat`, `mdamage`, `mecho`, `mechoaround`, `mexp`, `mforce`, `mgoto`, `mgold`, `mjunk`, `mkill`, `mcast`, `mchant`, `mperform`, `mmobflag`, `mload`, `mobjflag`, `mpurge`, `mroomflag`, `msave`, `msend`, `mskillset`, `mteleport`, `masound`, `log` | DG mob-script verbs; replaced by Lua bindings. |
| `qui` | Backup spelling of `quit`; trivial alias. |
| `z001#@#` | Internal sentinel, not user-facing. |
| `bless` | `do_wizutil` slot — admin spellcast already covered by `cast`. |
| `inspect` (`do_not_here`) | Legacy room-stub; if needed it'll be re-added per shop. |
| `dump`, `check`, `exchange`, `receive`, `rent`, `stone`, `value`, `disappear`, `appear` | All legacy `do_not_here` stubs that fire only in special rooms; port per-shop as needed. |
| `tnum`/`tlist`/`tsearch` (DG legacy) | Lua replacement. |

---

## Trigger / Lua debugging affordances

The legacy stack relied on the DG scripting commands above. Now that Lua is the host, the *capabilities* still need to exist. Inventory of what's in fierymud-rs vs. what's missing:

### Already implemented in Rust (`crates/mud-server/src/commands/admin_inspect.rs`)
- ✅ `triggers` / `trigs` — list catalog or inspect `(zone, id)` (body, flags, fire stats).
- ✅ `tstat` — dump a trigger's metadata (separate from `triggers`).
- ✅ `firetrig <zone> <id> [<actor>]` — manually fire a trigger; useful for testing bodies without setting up a trigger.
- ✅ `scripterrors` / `scripterr [n]` — show recent in-memory `ScriptErrorLog` entries (replaces legacy `do_mob_log`).
- ✅ `lua <code>` — REPL-style: run an inline Lua snippet with `actor` bound to the caller. Same API surface as triggers.
- ✅ `treload` — async reload of triggers from the DB (admin_management.rs).
- ✅ MCP harness exposes: `mcp__fierymud__trigger_info`, `mcp__fierymud__trigger_errors`, `mcp__fierymud__trigger_stats`, `mcp__fierymud__reload_triggers`, `mcp__fierymud__fire_trigger`. See `/api/admin/triggers*` endpoints.

### Missing — would help debug Lua

- ❌ **List triggers attached to a specific entity.** `triggers` lists the catalog; there's no `triggers on mob <name>` or `triggers on room here`. The MCP `inspect_actor` / `inspect_mob` may surface this — verify. Legacy: `tstat <mob>`/`tstat <obj>`/`tstat room`.
- ❌ **Inspect Lua state / variables on an entity.** Legacy had `varset`/`varunset` for per-mob DG vars; the Lua equivalent (per-entity Lua tables / `state` storage) needs a viewer + setter. No `varset` / `varlist` / `varclear` in modern.
- ❌ **Trigger-firing stack trace.** When `scripterrors` shows a fail, there's no way to ask "give me the full stack at fire site" — only the captured error. A `trace <error_id>` would help. Compare to legacy where you'd `tstat` then re-`firetrig` to repro.
- ❌ **Live attach/detach during a session.** Legacy `attach`/`detach` let you bolt a trigger onto a running mob/obj/room without touching DB. In Rust, you must edit the trigger row and `treload`. A `trig-attach <zone> <id> here` style command would be useful for builder iteration.
- ❌ **Pause-on-trigger / step debugging.** Neither exists, but Lua's `mlua` supports debug hooks. A `trigbreak <zone> <id>` that pauses on next fire and dumps locals would be a powerful builder tool.
- ❌ **Trigger-event history per entity.** A ring-buffer of "trigger X on mob Y fired at tick Z, returned ok / err" — `trighistory <mob>`. Legacy had partial coverage via syslog grepping.
- ❌ **Lua module / require browser.** If `mlua` is loading helper modules from disk, no command surfaces "what's loaded, what version". Useful as the helper library grows.
- ❌ **Trigger source from the catalog.** `triggers <zone> <id>` shows the body — but not the original DB row id, the `last_modified_at`, or who edited it. Tighten the dump to include audit fields when triggers are edited via muditor.

### Existing-but-thin

- ⚠️ `scripterrors` is in-memory only; on restart the log clears. Persisting recent errors to a DB table would let muditor render them as a builder dashboard.
- ⚠️ `firetrig` synthesizes an actor argument but doesn't accept a fully-built event payload. Triggers that key off `arg`, `cmd`, or specific event types may not exercise the right path. A `firetrig --event speech --arg "hello"` form is the missing parity.

---

## Modern-only additions

Commands that exist in fierymud-rs without a direct legacy ancestor:

- `account` — show account-level info (the modern multi-character account model).
- `achievements` / `achieve` — achievement system (no legacy equivalent).
- `apply` (admin) — apply an effect to an entity.
- `astat` — actor stat dump (analogue to `mstat` but unified for actors).
- `chants` (info list) — separate from `songs`/`spells` listing.
- `clientinfo` — terminal/client capability info.
- `cooldowns` / `cd` — cooldown system (legacy didn't have this category).
- `dumpworld` — dump world state for diagnostics.
- `firetrig` — see Trigger section.
- `hgoto` / `hgrant` / `hinfo` / `hrevoke` — house-system admin (replaces parts of `hcontrol`).
- `holylight` / `showids` — admin viewing toggles.
- `idle` — show your own idle stats (was a syslog-only thing in legacy).
- `lua` — inline REPL.
- `pk` — PK toggle / status.
- `pnote` / `playernote` — admin notes on a player (legacy had `note` but more global).
- `qaccept` / `qload` / `qgive` / `qcomplete` / `questinfo` — modern quest verbs (legacy had `qadd`/`qdel`/`qstat`/`qlist`/`quest`).
- `richtest` — test rich-text rendering for ANSI/MXP/etc.
- `roles` — role-based-permission listing (modern user-role system).
- `scripterrors` / `scripterr` — see Trigger section.
- `setweather` — admin weather set.
- `style` — output style preference.
- `treload` — see Trigger section.
- `triggers` / `trigs` — see Trigger section.
- `unalias` — pair to `alias` (legacy lacks an explicit unalias verb).
- `unignore` — pair to `ignore`.
- `wealth` / `gold` / `money` / `coins` / `wallet` — coalesced money command (legacy had only `score` and shop verbs).

---

## File pointers

- Legacy command table: `/home/strider/Code/mud/fierymud_legacy/src/interpreter.cpp:354-996`
- Rust commands directory: `/home/strider/Code/mud/fierymud-rs/crates/mud-server/src/commands/`
- Trigger debugging surface: `/home/strider/Code/mud/fierymud-rs/crates/mud-server/src/commands/admin_inspect.rs:200-280` and `:1100+`
- MCP admin endpoints: documented in `/home/strider/Code/mud/CLAUDE.md` ("MCP harness target switching")
