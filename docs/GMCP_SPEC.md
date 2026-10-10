# GMCP server-side spec

What `fierymud-rs` needs to emit so the Mudlet client's panels light up.

The client (`FierymudRs` package) is a passive consumer — it never
asks the server "send me Char.Vitals", it just reads `gmcp.Char.Vitals`
whenever Mudlet's GMCP receiver populates it. The server is
responsible for emitting the right frames at the right cadence; the
client wires a small set of named handlers to act on them.

Each section below lists:
- **Package name** — the GMCP package path (`Char.Vitals`, `Comm.Channel.Text`, …).
- **When to emit** — the cadence that keeps panels current.
- **Shape** — TypeScript-ish for clarity. All payloads are JSON.
- **Consumer** — the Lua file that reads the frame.
- **Status** — `Live` (working in the test scenarios), `Planned` (referenced
  in code but not yet wired server-side), or `Wanted` (would unblock a UI
  feature that's currently impossible).

The legacy server uses largely the same shapes; this doc is the
canonical client-side contract for the Rust port.

---

## Character identity & state

### `Char.Status`  — **Live**

Identity strip + tracker baseline.

| Field    | Type    | Notes                                       |
|----------|---------|---------------------------------------------|
| `name`   | string  | Character name                              |
| `class`  | string  | Full class name (`"Sorcerer"`)              |
| `level`  | number  | 1–100; 100+ flips to `GOD` in the UI        |
| `xp`     | number  | Lifetime XP — drives Tracker delta + rate   |
| `wealth` | number  | Lifetime gold — drives Tracker delta + rate |
| `race`   | string  | Optional (reserved; unused today)           |

**Cadence:** On login + on any change to the listed fields (level-up,
class change, wealth tick). At minimum, re-emit on every prompt so
the Tracker can compute XP/hr without integrating gaps.

**Consumer:** `src/scripts/FierymudRs/Vitals/Vitals.lua` (identity)
+ `Tracker/Tracker.lua` (XP/wealth delta).

### `Char.Vitals`  — **Live**

Main vitals gauges + level-progress for Tracker TTL.

| Field             | Type    | Notes                                              |
|-------------------|---------|----------------------------------------------------|
| `hp`              | number  | Current HP                                         |
| `max_hp`          | number  | Max HP                                             |
| `mp`              | number  | Current mana (0 for non-casters)                   |
| `max_mp`          | number  | Max mana (0 for non-casters → client hides gauge)  |
| `mv`              | number  | Current move/stamina                               |
| `max_mv`          | number  | Max move/stamina                                   |
| `next_level_pct`  | number  | % progress to next level (0..100, 100 = pre-ding)  |
| `string`          | string  | Pre-formatted prompt body (`H:hp/max_hp M:mp/max_mp V:mv/max_mv`) |

**Cadence:** Sent on the first prompt, after `Core.Hello` /
`Core.Supports.Set`, and whenever a field changes (including the
mid-round push when damage lands). An unchanged prompt sends nothing;
the client reads the cached `gmcp.Char.Vitals` on `onPrompt`.

**Consumer:** `Vitals/Vitals.lua`, `Vitals/Guages.lua`, `Tracker/Tracker.lua`.

---

## Combat

### `Char.Combat`  — **Live**

Tank + opponent + (optional) the viewer's current target for the
bottom-left TARGET panel.

```ts
{
  tank: {
    name: string,
    hp: number,
    max_hp: number,
  },
  opponent: {
    name: string,
    hp_percent: number,   // 0..100, server reports % only
  },
  target?: {
    name: string,
    hp_percent: number,
  },
}
```

`target` is the player's current swing (`Fighting`); `opponent` is
the group's main mob (today both are the same — they diverge when a
group-main concept lands). When `target` matches `opponent`, render
one combat row; when they differ, stack `Opponent: X` above
`Target: Y` with the Target row in a brighter accent.

**Cadence:** On change only (plus after renegotiation). Empty `{}`
when out of combat (the client uses that as a hide signal); it is sent
once when combat ends, not on every prompt.

**Consumer:** `Vitals/Guages.lua` `updateCombat()`.

### `Char.Aggro`  — **Live**

Threats panel (top of the THREATS section).

```ts
{
  hating: string[],      // mobs actively attacking / chasing
  remembering: string[], // mobs that walked away but remember
}
```

**Cadence:** Only emit when at least one array is non-empty. The
server gates emission; the client's render is null-safe but the panel
is hidden when the frame is absent.

**Consumer:** `Vitals/Guages.lua` `updateAggro()`.

### `Char.Effects`  — **Live**

Active buffs/debuffs — top center icon bar.

```ts
Array<{
  id: string,          // stable machine key: lowercased ability plain_name
                       // ("stone_skin"), or the lowercased effect name when no
                       // ability spawned it. Key icons / tiles on this.
  name: string,        // display label ("Stone Skin"); may be reworded, don't key on it
  ability: string,     // originating spell's display name ("Stone Skin"), "" when none
  duration: number,    // seconds remaining; -1 = permanent
  source: string,      // who cast it ("Self", "Mejna", etc.)
  strength: number,    // 1..n stacking strength
}>
```

**Cadence:** Sent at login (and after renegotiation / takeover), pushed
the tick an effect is added, removed or restacked (even if the player
receives no other output), and re-sent on a prompt when a duration
changed. An empty array `[]` means no active effects. Snapshot every
effect each emission — the client diffs by `id`. Permanent effects
(`duration: -1`) tick at -1 forever; the client just doesn't decrement
them.

**Consumer:** `Effects/Effects.lua`.

---

## Group / Party

### `Group`  — **Live**

Party panel mid-left.

```ts
{
  group_name: string,    // display name ("X's group")
  leader: string,        // name of the party leader
  count: number,         // member count; 0 or missing = solo
  members: Array<{
    name: string,
    with_leader: boolean,  // "here in the leader's room"
    level: number,
    race: string,
    class: string,         // first 3 chars used for tinting ("Sor", "War", "Pri")
    stats: {
      hp: number, max_hp: number,
      mp: number, max_mp: number,
      mv: number, max_mv: number,
    },
  }>
}
```

**Cadence:** On change only (plus after renegotiation), so member HP
changes re-send it. A solo player gets an empty `{}` once — the client
hides the panel.

**Consumer:** `Vitals/Guages.lua` `updateGroup()`.

---

## Room

### `Room.Info`  — **Live**

Map widget + current-room tracking.

```ts
{
  num: number,             // composite room key: zone * 100000 + id
  name: string,            // room title
  zone: string,            // zone display name ("" for god zones)
  area: string,            // same as `zone` (older clients)
  desc: string,            // room description, colour tags stripped
  environment: string,     // sector enum label ("Forest", "Inside", "City", "Mountains", ...)
  exits: { [direction: string]: number },  // dir → adjacent room num
  doors: { [direction: string]: string },  // dir → "closed" | "locked"; absent = no door or open door
  exit_details: {          // richer per-exit view; same keys as `exits`
    [direction: string]: {
      to: number,          // adjacent room num
      door?: true,         // present when the exit is a door
      door_name?: string,  // first door keyword ("gate")
      door_state?: "open" | "closed" | "locked",  // present with `door`
    }
  },
  coords?: string,         // optional "x,y,z" — preferred over compass-walk inference
}
```

Undiscovered hidden exits are omitted; one revealed by `search` appears
(and `Room.Info` is re-sent) as soon as it is found. In a dark room the
payload is `{}`.

**Cadence:** On room entry + look, sent *before* the room text. Re-sent when
the content changes (door opened/closed/locked, hidden exit found). Coordinates are optional but
strongly preferred — without them, the mapper falls back to dead
reckoning (offsets from the previous room by the player's last
movement direction), which gets corner cases wrong.

**Consumer:** `Mapper/Mapper.lua`.

### `Room.Players`  — **Live**

"Who else is in this room" strip (header bar inside the chat panel).

```ts
Array<{
  name: string,
  full_name: string,    // same as name today; reserved for color-bearing display form
  posture?: string,     // standing|sitting|kneeling|resting|sleeping|flying (omitted for a shape seen by infravision)
  status?: string,      // "stunned" while held (paralysis, mesmerize or stun)
  hold?: string,        // with status: paralyzed|mesmerized|stunned
  // potentially: class, level, with_leader — currently unused
}>
```

The text `look` lists each of these players on a line of their own with the
same state ("Bob is sleeping here.").

**Cadence:** On room entry + on player connect/disconnect in the
current room. Empty array shows `(no one else here)`.

**Consumer:** `Vitals/Guages.lua` `updateRoomPlayers()`.

### `Room.AddPlayer` / `Room.RemovePlayer`  — **Live**

Diff events for incremental updates so the server doesn't need to
re-emit the full snapshot on every step.

```ts
// AddPlayer
{ name: string }
// RemovePlayer
{ name: string }
```

The client's handler ignores the diff payload and just re-reads
`gmcp.Room.Players` (Mudlet's GMCP receiver mutates the table
in-place before firing the event). So the server can either:

- Emit a fresh `Room.Players` snapshot AND nothing else, or
- Mutate `Room.Players` in-place AND emit `Room.AddPlayer` /
  `Room.RemovePlayer` for the diff — either works.

---

## Chat / Communication

### `Comm.Channel.Text`  — **Live**

Every channel utterance (gossip, tell, group, shout, wiznet, …).

```ts
{
  channel: string,   // lowercase channel name: "gossip" | "tells" | "group" | "shout" | "wiznet" | ...
  talker: string,    // speaker's name (or "a herald" / "an angry guard" for NPCs)
  text: string,      // message body, **plain text** — server strips color codes
}
```

**Cadence:** On every channel send, including the player's own
messages (the client formats `talker == self.name` differently for
self-mention highlighting).

**Consumer:** `Chat/Chat.lua` `onCommChannelText()`. Channel names map to
chat tabs via `channelTabs` (gossip→Gossip, shout→Local, wiznet→Wiz, …).

### `Comm.Channel.List`  — **Live**

Optional channel directory — replaces the hardcoded client-side
list with a server-aware one. When the server sends this, the
client rebuilds its tab routing table.

```ts
Array<{
  name: string,       // canonical key (matches `channel` in Comm.Channel.Text)
  caption?: string,   // pretty name for the tab label; defaults to `name`
  command?: string,   // command the player runs to use this channel; for future tab-click-to-target
}>
```

**Cadence:** Once on login. Re-emit if the player gains/loses access
(e.g., immortal promotion, clan join, quest channel unlock).

**Consumer:** `Chat/Chat.lua` `onCommChannelList()`.

---

## Inventory

### `Char.Items.List`  — **Live**

Inventory + equipment panels.

```ts
{
  location: "inv" | "wear",   // selects which panel to update
  items: Array<{
    id: string,               // session-scoped runtime id; not stable across server restarts
    name: string,             // display name with article ("a glittering ruby ring")
    keyword?: string,         // optional — if absent, client uses the last word of `name`
    type: string,             // "weapon" | "armor" | "container" | "scroll" | "potion" | ...
    identified: boolean,      // shows a `*` marker before the name in the panel
    location?: string,        // worn slot ("head", "neck", "finger (left)", ...) — emitted only when the outer `location` is `"wear"`
  }>
}
```

The client requests this on init by sending `Char.Items.Inv` outbound.
Server should respond with `Char.Items.List` for both `inv` and
`wear` locations on login + after any inventory mutation (get / drop
/ wear / remove / give / quaff / etc.).

**Consumer:** `Inventory/Inventory.lua`.

---

### `Room.Mobs`  — **Live**

Every mob in the current room, with a `hostile` flag and service
`professions`. Drives both the threat panel (filter `hostile:true`)
and the friendly-NPC panel (filter `hostile:false`).

```ts
Array<{
  id: string,             // session-scoped runtime id; pass back to Room.Mob.Get for detail. Not stable across server restarts.
  name: string,           // display name ("a vicious goblin")
  hostile: boolean,       // currently engaged, hates/remembers viewer, OR alignment ≤ aggro threshold
  hp_percent: number,     // 0..100; emit for all mobs (client can hide bar on friendlies)
  targeting: string | null, // who the mob is swinging at; null when not engaged
  posture: string,        // standing|sitting|kneeling|resting|sleeping|flying
  status?: string,        // "stunned" while held (more later: casting / fleeing)
  hold?: string,          // with status: paralyzed|mesmerized|stunned
  professions: string[],  // ["shop","bank","inn","mail","guild","trainer"] — empty array on plain mobs
}>
```

`professions` strings match the keys in `Room.Services.services` and
are the routing keys for service-related UI affordances (a shop
icon, a bank button, etc.).

**Visibility:** The frame is what the viewer *can see*, using the same
rules as the text `look`: mobs that are magically invisible (unless the
viewer has detect_invisible, `HOLY_LIGHT` or is a god) or whose
`WizInvis` level is above the viewer's level are filtered out
server-side. In a dark room the array is empty, except that a viewer
with infravision gets one generic entry per character
(`name: "the red shape of a medium living being"`, `hostile:false`,
`hp_percent:0`, no professions); `Room.Mob.Get` on it is a no-op.
`Room.Players`, `Room.AddPlayer`/`RemovePlayer`, `Room.Info` and
`Char.Aggro`/`Char.Combat` names follow the same predicate (an unseen
opponent is `"someone"`).

**Cadence:** On change only (plus after renegotiation), and always
alongside `Room.Info` on `look` (so every move). Empty array clears the
panels.

**Consumer:** Threat panel + friendly-NPC panel (to be wired).

### `Room.Services`  — **Live**

Derived room-level service summary. Union of every present mob's
`professions`, deduped, insertion-stable. Lets the client paint a
service chip on the room header without walking `Room.Mobs`.

```ts
{
  services: string[],   // ["shop","bank","inn","mail","guild","trainer"]
}
```

Service tag mapping (server-side, from `MobProfession`):
- `Shopkeeper`   → `"shop"`
- `Banker`       → `"bank"`
- `Receptionist` → `"inn"`
- `Postmaster`   → `"mail"`
- `Guildmaster`  → `"guild"`
- `Trainer`      → `"trainer"`

**Cadence:** Same as `Room.Mobs`. Empty array means no services here.

**Consumer:** Room-header chips (to be wired).

### `Room.Mob.Get` / `Room.Mob.Info`  — **Live**

Click-to-detail request/response. Client sends `Room.Mob.Get` with
an id from `Room.Mobs[i].id`; server replies with `Room.Mob.Info`.

**Server-side gates** (all enforced; mismatch → silent no-op so a
client brute-forcing entity ids can't distinguish failure modes):
- mob entity must still exist (handles despawn-mid-frame)
- mob must be in the requesting player's room (no cross-map snoop)
- mob must be visible to the viewer (invisible, `WizInvis` above
  viewer's level, or the room is dark to them → no info leak, even
  with a valid id from a prior frame)

**Outbound** (client → server):
```ts
{ id: string }
```

**Inbound** (server → client) — `Room.Mob.Info`:
```ts
{
  id: string,              // echoes the requested mob id (session-scoped, like Room.Mobs[i].id)
  name: string,
  description: string,
  professions: string[],
  shop?: {
    items: Array<{
      id: string,          // "<zone>:<id>" of the object proto — stable across restarts (content key)
      name: string,        // display name with article ("a glittering ruby ring")
      keyword: string,     // canonical noun for command targeting (`buy <keyword>`); empty when proto has no keywords
      price: number,       // copper; 0 means "use proto base × buy_profit"
      stock: number,       // -1 = unlimited
    }>,
    accepts: string[],     // ObjectType strings the shop will buy from the player
  },
}
```

Two ID schemes coexist intentionally: top-level `id` is the live mob
entity (session-scoped, unstable across restarts); `shop.items[i].id`
is the content-key `"<zone>:<id>"` (stable, useful for client-side
caching of item details).

`shop` is present only when the mob is registered in `ShopCatalog`
(keeper of a defined shop). Future blocks (`trainer`, `bank`, …)
follow the same optional-key pattern.

```ts
  inn?: {
    inn_name: string,
    tiers: Array<{
      name: string,        // rent <name> argument (case-insensitive)
      tier: number,        // 1..3 (basic / suite / penthouse)
      fee_gp: number,      // flat charge in gold
      affordable: boolean, // precomputed vs viewer's on-hand Wealth
    }>,
    current_rest: {        // null when the viewer holds no RestState
      source: string,      // "NONE" | "QUIT" | "CAMP" | "INN" | "HOUSE"
      tier: number,
      repose: number,      // sticky repose-point pool
    } | null,
  }
```

`inn` is present only when the mob carries `MobProfession::Receptionist`
*and* its room has an `InnRoom`. The rental data lives on the room
(where `rent` reads it); the server hops mob → room → `InnRoom` to
build the block. `affordable` and `current_rest` are viewer-scoped, so
the block is built per requesting player. To rent, the client sends the
plain `rent <name>` command — tiers 2-3 trigger a server-side y/n
confirm that lands in the main console.

```ts
  bank?: {
    on_hand: number,    // copper, viewer's Wealth
    per_char: number,   // copper, viewer's BankWealth
    account: number,    // copper, viewer's AccountWealth (shared across the account's characters)
  }
```

`bank` is present when the mob carries `MobProfession::Banker`. All
three pools are viewer-scoped — each pool is the requesting player's
own balance, taken at the moment of the request. The server has no
`deposit all`/`withdraw all` keyword; the client uses the GMCP pool
values to compute "all" amounts at click time and sends a numeric
`deposit <n>` / `withdraw <n>` / `adeposit <n>` / `awithdraw <n>`.
A second click refreshes the snapshot.

**Cadence:** On demand — one `Room.Mob.Info` per `Room.Mob.Get`.

**Consumer:** Mob detail popover — shop + inn + bank blocks wired.

### `Room.Mail.Inbox`  — **Live**

Async follow-up to a `Room.Mob.Get` on a Postmaster. Ships as a
separate GMCP frame (not a `mail?` sub-block on `Room.Mob.Info`)
because the inbox is a DB fetch and `handle_room_mob_get` is
sync — the server `tokio::spawn`s the fetch and pushes this
frame when it lands, typically within one round-trip.

```ts
{
  mob_id: string,    // echoes the Room.Mob.Get target id; lets the client
                     // correlate this inbox with the popup that requested it
  unread: number,
  total: number,
  messages: Array<{
    id: number,      // mail row PK, stable across session
    slot: number,    // 1-based, matches `mailbox` listing
    sender: string,  // sender's display name
    subject: string,
    sent_at: string, // "YYYY-MM-DD HH:MM"
    unread: boolean,
  }>,                // capped at 50 most recent (newest first)
}
```

The client correlates by `mob_id`: if the popup is still showing
the originating Postmaster, the inbox is merged in and the popup
re-rendered. A stale frame (player closed the popup or clicked
another mob) is silently dropped so the popup doesn't pop back
up.

To act on a row, the client sends the existing line commands:
`readmail <slot>` / `delmail <slot>`. Compose flow stays
text-driven via `mail <recipient>` — the popup pre-fills the
input bar but the multi-line composition session continues in
the main console.

**Cadence:** On demand — at most one `Room.Mail.Inbox` per
`Room.Mob.Get` on a Postmaster.

**Consumer:** Mob detail popover — wired.

### `Char.Skills`  — **Live**

Per-skill cooldown + available flag for the (future) skill-bar
widget. One entry per known ability.

```ts
{
  skills: Array<{
    name: string,
    cooldown: number,   // seconds remaining; 0 = available
    available: boolean, // mirror of cooldown == 0; precomputed for cheap filtering
    passive: boolean,   // true = works automatically (dual wield, weapon skills); no button
    mp_cost?: number,   // reserved — not emitted today (cost is circle-derived)
  }>
}
```

`passive` comes from the ability row's `passive` tag (`Ability.tags`).
Clients should not draw an activation chip for `passive: true` skills.

**Cadence:** On change only (plus after renegotiation). Cooldown
countdowns change the payload, so it re-sends each second while any
skill is cooling down.

**Consumer:** Skill bar (to be wired).

Distinct from `Char.Skills.List` (flat array of names emitted in
response to client `Char.Skills.Get`) — that's the legacy IRE
directory; `Char.Skills` is the liveness feed.

---

## Wanted — server work needed to unlock UI features

*(Nothing currently. Wanted entries graduated to Live land here when
new client-side UX requests turn up.)*

---

## Outbound (client → server)

- `Char.Items.Inv` — at startup, asks the server to send a fresh
  `Char.Items.List` for `inv` + `wear`. Server should treat this as
  a "snapshot please" request.

- `Char.Skills.Get` — asks for the flat `Char.Skills.List` directory
  (separate from the `Char.Skills` liveness feed).

- `Room.Mob.Get` — `{ id: string }` from a `Room.Mobs[i].id`. Server
  replies with `Room.Mob.Info` if the mob is in the requesting
  player's room; silent no-op otherwise.

- `MRResult` — internal client→test-runner channel (see
  `docs/AGENT_DEV.md`). Server can ignore.

- `Test.Result` — same; safe to ignore server-side.

There's no `Core.Supports.Add` round-trip — the server can emit any
of the packages above unconditionally and the client will consume
what it knows.

---

## Implementation notes for the Rust side

- **Field naming:** All payload keys are snake_case across the
  contract (`max_hp`, `hp_percent`, `next_level_pct`, `full_name`,
  `group_name`, `application_id`, `small_image`, `start_time`).
  We diverge from IRE/Mudlet's stock camelCase — the client is
  fully custom, so consistency beats community precedent.
  Server keys and client keys must match exactly; the client does
  no normalization.

- **Empty-frame semantics:** Many packages use "absent or empty" to
  mean "hide this UI section." Prefer sending an empty object `{}`
  for `Group` and `Char.Combat` over omitting the frame entirely —
  it makes the client's clear-on-transition logic cleaner.

- **Frame frequency:** Don't worry about over-emitting. The client
  uses cached state + diffs at the render layer; identical frames
  are no-ops.

- **JSON encoding:** Plain UTF-8 JSON in the GMCP body (everything
  after `Package.Name ` and before `IAC SE`). The client uses
  Mudlet's bundled `yajl` parser which is tolerant of whitespace and
  trailing commas but strict about quoting.

- **Testing the contract:** Each package above has a corresponding
  scenario in `scripts/scenarios/`:
  - `vitals_low_hp.txt` — Char.Status + Char.Vitals
  - `effects_full.txt` — Char.Effects
  - `group_and_aggro.txt` — Group + Char.Aggro
  - `combat_engaged.txt` — Char.Combat + Char.Aggro
  - `chat_channels.txt` — Comm.Channel.Text
  - `room_explored.txt` — Room.Info

  Run any scenario against the mock daemon and screenshot the result
  to validate the exact bytes the client expects.
