use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState, Sector};

/// Marker: this entity is a zone (organizational grouping of rooms).
#[derive(Component, Debug, Clone, Copy)]
pub struct Zone;

/// Per-zone climate. Loaded from `Zones.climate` at startup. Used by
/// `cmd_weather` to render atmospheric flavor for the player's
/// current zone (snowfall in subarctic, dust storms in arid, etc.).
#[derive(Component, Debug, Clone, Copy)]
pub struct ZoneClimate(pub mud_db::enums::Climate);

/// Marker: this entity is a room.
#[derive(Component, Debug, Clone, Copy)]
pub struct Room;

/// Marker: this entity is a player character.
#[derive(Component, Debug, Clone, Copy)]
pub struct Player;

/// Marker: this entity is currently connected (logged in and online).
#[derive(Component, Debug, Clone, Copy)]
pub struct Online;

/// Account ownership and authorization data, stamped onto the Player entity
/// at login. `role` comes from the Users row; `perms` and `character_id`
/// come from the Characters row. `character_id` is what save-on-disconnect
/// uses to write state back.
#[derive(Component, Debug, Clone)]
pub struct Account {
    pub user_id: String,
    pub character_id: String,
    pub role: mud_db::enums::UserRole,
    pub perms: Vec<mud_db::enums::Permission>,
}

/// Marker: this entity is a non-player mob/NPC instance.
#[derive(Component, Debug, Clone, Copy)]
pub struct Mob;

/// Loot-claim window on a freshly-spawned mob corpse. Only
/// `owner` (and their group, once group bridging lands) can `get`
/// items from this corpse until `expires_at` — past that,
/// anyone can loot. Player-death corpses don't carry this; the
/// dead player's group is whoever's standing there.
#[derive(Component, Debug, Clone, Copy)]
pub struct LootClaim {
    pub owner: Entity,
    pub expires_at: std::time::Instant,
}

/// Per-instance copy of the prototype's `behaviors` list. Drives
/// the runtime AI gates (Sentinel skips wander, Helper joins
/// allies in combat, Scavenger picks up loot, etc.). Empty for
/// mobs whose proto carries no behaviors; the inherent default
/// is "no AI flag — react only to direct events".
#[derive(Component, Debug, Clone, Default)]
pub struct MobBehaviors(pub Vec<mud_db::enums::MobBehavior>);

impl MobBehaviors {
    /// True when the mob carries the given behavior flag. Linear
    /// scan — n is ≤ 4 in practice (most mobs have 0-2 flags).
    #[must_use]
    pub fn has(&self, flag: mud_db::enums::MobBehavior) -> bool {
        self.0.contains(&flag)
    }
}

/// Marker: this entity is an item instance (weapon, potion, container, …).
#[derive(Component, Debug, Clone, Copy)]
pub struct Item;

/// Marker: a Light-type item is currently lit. Display-only today;
/// once room-darkness mechanics land, this component on a held/worn
/// item will let the carrier see in dark rooms.
#[derive(Component, Debug, Clone, Copy)]
pub struct Lit;

/// Per-instance fuel for a Light-type item. Decremented once per
/// game-hour while `Lit` is set; when `remaining` hits 0 the
/// `Lit` marker is removed and a "the X gutters out" line is
/// broadcast to the room. `remaining < 0` is sentinel for
/// infinite (matches schema's `values.Remaining = -1`).
#[derive(Component, Debug, Clone, Copy)]
pub struct LightFuel {
    pub capacity: i32,
    pub remaining: i32,
}

/// Lookup keywords for an entity, used by `get`/`drop`/`attack` matching.
/// First entry is typically the canonical identifier ("sword"), followed by
/// other words a player might type ("rusty", "iron").
#[derive(Component, Debug, Clone)]
pub struct Keywords(pub Vec<String>);

/// Equipment slots a wearable item can occupy. Order roughly head-to-toe
/// for `equipment` display; weapons and odd-fits at the end. Modeled as
/// single slots even when the schema flag implies a pair (a single
/// `Ears` slot covers both ears, a single `Wrist` slot covers both
/// wrists) — matches the legacy `CircleMUD` shape and avoids needing
/// per-side bookkeeping in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Head,
    Eyes,
    Face,
    Ears,
    Neck,
    About,
    Body,
    Arms,
    Wrist,
    Hands,
    LeftFinger,
    RightFinger,
    Waist,
    Legs,
    Feet,
    Wield,
    Hold,
    Light,
    Hover,
    Badge,
}

impl Slot {
    pub const ORDER: &'static [Self] = &[
        Self::Head,
        Self::Eyes,
        Self::Face,
        Self::Ears,
        Self::Neck,
        Self::About,
        Self::Body,
        Self::Arms,
        Self::Wrist,
        Self::Hands,
        Self::LeftFinger,
        Self::RightFinger,
        Self::Waist,
        Self::Legs,
        Self::Feet,
        Self::Wield,
        Self::Hold,
        Self::Light,
        Self::Hover,
        Self::Badge,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Eyes => "eyes",
            Self::Face => "face",
            Self::Ears => "ears",
            Self::Neck => "neck",
            Self::About => "about body",
            Self::Body => "body",
            Self::Arms => "arms",
            Self::Wrist => "wrist",
            Self::Hands => "hands",
            Self::LeftFinger => "finger (left)",
            Self::RightFinger => "finger (right)",
            Self::Waist => "waist",
            Self::Legs => "legs",
            Self::Feet => "feet",
            Self::Wield => "wielded",
            Self::Hold => "held",
            Self::Light => "light",
            Self::Hover => "hovering",
            Self::Badge => "badge",
        }
    }

    /// Canonical `SCREAMING_SNAKE_CASE` name for DB persistence — matches
    /// the `equipped_location` text we write to `CharacterItems`.
    #[must_use]
    pub fn db_label(self) -> &'static str {
        match self {
            Self::Head => "HEAD",
            Self::Eyes => "EYES",
            Self::Face => "FACE",
            Self::Ears => "EARS",
            Self::Neck => "NECK",
            Self::About => "ABOUT",
            Self::Body => "BODY",
            Self::Arms => "ARMS",
            Self::Wrist => "WRIST",
            Self::Hands => "HANDS",
            Self::LeftFinger => "FINGER_LEFT",
            Self::RightFinger => "FINGER_RIGHT",
            Self::Waist => "WAIST",
            Self::Legs => "LEGS",
            Self::Feet => "FEET",
            Self::Wield => "WIELD",
            Self::Hold => "HOLD",
            Self::Light => "LIGHT",
            Self::Hover => "HOVER",
            Self::Badge => "BADGE",
        }
    }

    /// Parse the free-text `equipped_location` from `CharacterItems`. Case
    /// insensitive. Mirrors `db_label` round-trip plus a few legacy
    /// aliases (`NECK_1`/`NECK_2` collapse to `NECK`, `FINGER_R`/`FINGER_L`
    /// map to `LeftFinger`/`RightFinger`). Returns None for any label
    /// we still don't model.
    #[must_use]
    pub fn from_label(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "HEAD" => Some(Self::Head),
            "EYES" => Some(Self::Eyes),
            "FACE" => Some(Self::Face),
            "EARS" | "EAR" => Some(Self::Ears),
            // Schema sometimes splits NECK_1/NECK_2 — we collapse to one
            // slot for now since the runtime doesn't model the pair.
            "NECK" | "NECK_1" | "NECK_2" => Some(Self::Neck),
            "ABOUT" => Some(Self::About),
            "BODY" => Some(Self::Body),
            "ARMS" => Some(Self::Arms),
            "WRIST" => Some(Self::Wrist),
            "HANDS" => Some(Self::Hands),
            "FINGER_LEFT" | "FINGER_R" => Some(Self::LeftFinger),
            "FINGER_RIGHT" | "FINGER_L" => Some(Self::RightFinger),
            "WAIST" | "BELT" => Some(Self::Waist),
            "LEGS" => Some(Self::Legs),
            "FEET" => Some(Self::Feet),
            "WIELD" => Some(Self::Wield),
            "HOLD" => Some(Self::Hold),
            "LIGHT" => Some(Self::Light),
            "HOVER" => Some(Self::Hover),
            "BADGE" => Some(Self::Badge),
            _ => None,
        }
    }
}

/// Item-only: the slot this item is wearable in. Items without a
/// `WearableIn` component aren't wearable at all.
#[derive(Component, Debug, Clone, Copy)]
pub struct WearableIn(pub Slot);

/// Item-only, only present while the item is equipped: which slot it
/// occupies. Combined with Located(wearer) means "X has Y equipped in Z".
#[derive(Component, Debug, Clone, Copy)]
pub struct EquippedSlot(pub Slot);

#[derive(Component, Debug, Clone, Copy)]
pub struct Health {
    pub hp: i32,
    pub max: i32,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct Stamina {
    pub current: i32,
    pub max: i32,
}

/// Hunger gauge — game-hours since the last meal. 0 = sated. Once
/// the value crosses a threshold (legacy `CircleMUD`: 48 hours), the
/// hunger tick (TBD) starts draining stamina then HP. Loaded from
/// `Characters.hunger` at spawn; persisted on disconnect. Wiring is
/// load + save only today; the tick consumer arrives in a follow-up.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Hunger(pub i32);

/// Thirst gauge — game-hours since the last drink. Same contract
/// as `Hunger` with a tighter legacy threshold (~24 hours).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Thirst(pub i32);

#[derive(Component, Debug, Clone, Copy, Default)]
pub struct CombatStats {
    pub hit_roll: i32,
    pub dmg_roll: i32,
    pub ac: i32,
    pub alignment: i32,
}

/// `D&D`-style ability scores (3..=25 in classic `CircleMUD`; schema
/// defaults to 13). Bonus computed as `(score - 10) / 2`. Loaded
/// from `Characters` on player spawn; not yet on mobs (their stats
/// aren't in the schema yet).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct CoreStats {
    pub strength: i32,
    pub dexterity: i32,
    pub constitution: i32,
    pub intelligence: i32,
    pub wisdom: i32,
    pub charisma: i32,
}

impl CoreStats {
    /// Standard `(score - 10) / 2` bonus, integer-truncated. Negative
    /// for sub-10 scores. Used by ability formulas (`str_bonus`,
    /// `dex_bonus`, etc.).
    #[must_use]
    pub fn bonus(score: i32) -> i32 {
        (score - 10) / 2
    }
}

/// Combat state: this entity is currently fighting the target. Removed when
/// combat ends (death, flee, room mismatch).
#[derive(Component, Debug, Clone, Copy)]
pub struct Fighting(pub Entity);

/// Marker: this entity is stunned and skips combat swings. Inserted by
/// `stun` effects in `invoke_ability`; removed by `effects_tick` once
/// every backing `EffectInstance` named "stun" on the entity has
/// expired.
#[derive(Component, Debug, Clone, Copy)]
pub struct Stunned;

/// Marker: this player is dead-but-incorporeal and waiting for `release`.
/// Set by `combat::handle_death` after spawning the player's corpse;
/// removed by `cmd_release` which restores HP and moves the player to
/// their recall point. The marker doesn't currently gate any combat
/// behavior on its own — that's a follow-up. v1 just signals "your
/// body is over there, type release to return."
#[derive(Component, Debug, Clone, Copy)]
pub struct Ghost;

/// Marker: this Item is a player corpse. Holds items the dead player
/// was carrying as `Located` children; the corpse-decay tick despawns
/// it after `CorpseDecay.remaining_secs` reaches zero, dropping the
/// contained items to the room first.
#[derive(Component, Debug, Clone, Copy)]
pub struct Corpse;

/// Decay timer on a corpse. Decrements one second per game-second;
/// when it hits 0 the corpse despawns and any items Located on it
/// drop to the room. Default v1 lifetime is 10 minutes (600 secs);
/// configurable per-spawn for boss/quest corpses later.
#[derive(Component, Debug, Clone, Copy)]
pub struct CorpseDecay {
    pub remaining_secs: i32,
}

/// Marker: this entity is hidden / sneaking. Resolves the `hidden`
/// symbol in formula expressions to 1 (vs 0 when absent). Used by
/// rogue-style abilities (BACKSTAB's `bonusIfHidden`, future
/// THROATCUT bonus). No `hide` command yet — admin tooling /
/// future content sets the marker.
#[derive(Component, Debug, Clone, Copy)]
pub struct Stealth;

/// Per-item finite charges for `ObjectAbilities`-bound items
/// (wands, staves). `-1` means unlimited (never decremented).
/// `0` means depleted — the item should despawn or refuse to
/// activate. The runtime decrements on each successful invocation
/// via `wave` / `tap`.
#[derive(Component, Debug, Clone, Copy)]
pub struct Charges(pub i32);

/// Per-character wimpy threshold: when the WIMPY flag is set and
/// the character's HP falls below this percentage of max, the
/// combat tick auto-flees them. Default 25 (matches legacy
/// `FieryMUD`'s hardcoded value); the `wimpy <pct>` command sets a
/// different one.
#[derive(Component, Debug, Clone, Copy)]
pub struct WimpyThreshold(pub i32);

/// On-hand wealth in copper units. The schema column is BIGINT; we
/// keep it as `i64` so massive merchant-mob loot doesn't overflow.
/// Display denominations (platinum / gold / silver / copper) are
/// computed at render time by `cmd_wealth`.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Wealth(pub i64);

/// Practice-point pool. Spent by `practice <ability>` to bump
/// proficiency. Awarded on level-up (handler grants `level` points
/// per gain). Persisted to `Characters.skill_points`.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SkillPoints(pub i32);

/// Marks a mob as fronting a shop. The `(zone, id)` pair is the key
/// into `ShopCatalog.by_key`. Attached at mob-reset spawn time when
/// the mob's proto matches a row in the `keeper_index` lookup.
#[derive(Component, Debug, Clone, Copy)]
pub struct Shopkeeper {
    pub shop_zone_id: i32,
    pub shop_id: i32,
}

/// Component on spawned BOARD-typed items: the `Board.id` they
/// connect to (parsed from `Objects.values.Pages`). Examine renders
/// a hint pointing the player at `board <alias>` — the actual board
/// listing comes from a live DB query.
#[derive(Component, Debug, Clone, Copy)]
pub struct BoardLink(pub i32);

/// Lua trigger keys attached to a mob / object / room entity at
/// spawn time. Each tuple is the `(zone_id, id)` of a row in the
/// `TriggerCatalog` resource. The trigger dispatcher (not yet
/// wired) reads this list when an entity-relevant event fires —
/// movement into a room, mob kill, item use, etc. — and runs the
/// matching trigger bodies through `mlua`.
#[derive(Component, Debug, Clone, Default)]
pub struct AttachedTriggers(pub Vec<(i32, i32)>);

/// Marker: the player is flying. Movement treats every sector as
/// equally easy (sector cost 1 instead of 4-6 for water etc.) but
/// adds a flat +1 stamina per move on top — flying is great over
/// difficult terrain, mildly costly on roads. `walk` / `land` clear
/// the marker.
#[derive(Component, Debug, Clone, Copy)]
pub struct Flying;

/// Marker: this mob is mountable. Set on horse/warhorse-style mobs
/// at content time (currently auto-applied to mobs whose proto
/// keywords contain "horse" / "mount"; richer content metadata can
/// override later). `mount <mob>` only succeeds on Mountable mobs.
#[derive(Component, Debug, Clone, Copy)]
pub struct Mountable;

/// Component on a rider: which mount they're on. Movement updates
/// the mount's `Located` whenever the rider moves; the mount's own
/// movement is otherwise locked. Cleared by `dismount`.
#[derive(Component, Debug, Clone, Copy)]
pub struct Mounted(pub Entity);

/// Component on a mount: which rider is on top. Mostly for symmetry
/// (so we can render "X rides Y" / refuse double-mounting); the
/// real movement plumbing reads `Mounted` on the rider.
#[derive(Component, Debug, Clone, Copy)]
pub struct RiddenBy(pub Entity);

/// Pending group invite from another player. Set on the recipient
/// when an inviter runs `invite <name>`. `accept` triggers the
/// follow link; `decline` clears the marker. Cleared automatically
/// on disconnect since the component is session-scoped.
#[derive(Component, Debug, Clone, Copy)]
pub struct GroupInvite {
    pub from: Entity,
    pub at: std::time::Instant,
}

/// Component on spawned DRINKCONTAINER-typed items: per-instance
/// liquid state mirrored from `Objects.values`. `remaining` mutates
/// on `drink`/`sip`/`pour`/`fill`; the proto values are the
/// initial-spawn defaults only. Capacity caps `remaining` on `fill`.
#[derive(Component, Debug, Clone)]
pub struct LiquidContainer {
    /// Schema's `Liquid` enum label — `WATER` / `WINE` / `BEER` /
    /// etc. Stored as a string because the runtime only displays it.
    pub liquid: String,
    pub capacity: i32,
    pub remaining: i32,
    /// `true` when the schema's `Poisoned` flag was set; consumers
    /// (drink/sip) emit a poison line and can wire a real effect
    /// once `Effect`'s poison row is applied.
    pub poisoned: bool,
}

/// In-flight mail composition. Attached when the player runs
/// `mail <recipient>`; while present, every line of input is routed
/// to the mail-composition handler instead of normal dispatch. The
/// first non-blank line becomes the subject; subsequent lines append
/// to the body. `.send` finalizes (DB insert + remove component);
/// `.abort` discards; `.preview` echoes the current draft.
#[derive(Component, Debug, Clone)]
pub struct MailDraft {
    /// User id of the addressed account (resolved at `mail` time
    /// from the recipient character name).
    pub recipient_user_id: String,
    /// Human-readable label for the recipient — usually the
    /// character name as the player typed it. Echoed in messages.
    pub recipient_label: String,
    pub subject: Option<String>,
    pub body: Vec<String>,
}

/// In-flight message-board post or edit. Same shape as `MailDraft`
/// but addresses a `Board` row instead of a User. Resolved at
/// `post`/`editpost` time; composition flow is identical.
///
/// `edit_message_id = Some` switches the `.send` path to UPDATE +
/// audit-trail insert (`BoardMessageEdit`); `None` is a fresh post.
#[derive(Component, Debug, Clone)]
pub struct BoardDraft {
    pub board_id: i32,
    pub board_alias: String,
    pub board_title: String,
    pub subject: Option<String>,
    pub body: Vec<String>,
    pub edit_message_id: Option<i32>,
}

/// Bank balance in copper units, mirrored from `Characters.bank_wealth`.
/// Loaded at spawn, displayed by `cmd_balance`. No deposit/withdraw
/// commands are wired yet; once they land they'll need to round-trip
/// through `save_state`.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct BankWealth(pub i64);

/// Per-character ability cooldown table. Maps `Ability.id` → the
/// `Instant` at which the cooldown expires (i.e. the ability becomes
/// usable again). Inserted lazily on first cooldown set; entries past
/// their `ready_at` are stale and ignored. Session-scoped — not
/// persisted to the database.
#[derive(Component, Debug, Default, Clone)]
pub struct Cooldowns {
    pub ready_at: std::collections::HashMap<i32, std::time::Instant>,
}

/// Following state: this entity moves automatically when the target moves.
/// Distinct from Located — followers retain their own Located even while
/// chasing the leader, and the follow edge is preserved across rooms.
#[derive(Component, Debug, Clone, Copy)]
pub struct Follower(pub Entity);

/// Bodyguard pairing: this entity intercepts incoming swings that
/// were aimed at the target. Combat snapshots redirect swing.target
/// from `Guarding.0` onto the guarder when both are in the same
/// room. Set via `guard <target>`; cleared via `guard off`. The
/// Mounted/RiddenBy split is similar but for mount semantics.
#[derive(Component, Debug, Clone, Copy)]
pub struct Guarding(pub Entity);

/// Most recent sender of a `tell` to this entity. Used by `reply` to find
/// the previous correspondent. Cleared (or stale-checked) on the receiver
/// side, not the sender side.
#[derive(Component, Debug, Clone, Copy)]
pub struct LastTeller(pub Entity);

/// Read-only summary of the account behind this player session.
/// Snapshot taken at login from `Users` and `Characters`; powers the
/// `account` command without needing a DB roundtrip per query.
/// Stale-by-design — won't reflect characters created mid-session.
#[derive(Component, Debug, Clone)]
pub struct AccountSummary {
    pub email: String,
    pub display_name: String,
    /// (name, level) per character on this account, sorted as
    /// `Characters::list_for_user` returned them (level desc, name asc).
    pub characters: Vec<(String, i32)>,
}

/// Per-session ignore list. A player whose name appears here can't
/// reach this entity through `tell` — the sender gets a "they're
/// ignoring you" message and the receiver sees nothing. Stored as
/// case-insensitive lowercased names.
#[derive(Component, Debug, Default, Clone)]
pub struct IgnoreList(pub Vec<String>);

impl IgnoreList {
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        let lc = name.to_ascii_lowercase();
        self.0.contains(&lc)
    }

    /// Returns true if added, false if already present.
    pub fn add(&mut self, name: &str) -> bool {
        let lc = name.to_ascii_lowercase();
        if self.0.contains(&lc) {
            return false;
        }
        self.0.push(lc);
        true
    }

    /// Returns true if removed, false if not present.
    pub fn remove(&mut self, name: &str) -> bool {
        let lc = name.to_ascii_lowercase();
        let before = self.0.len();
        self.0.retain(|n| *n != lc);
        self.0.len() < before
    }
}

/// Bounded history of recent `tell` senders, newest first. Stores the
/// sender's name at the time (not their Entity) so the readout is
/// stable across reconnects / despawns. Display-only — `reply` still
/// uses `LastTeller`.
#[derive(Component, Debug, Default, Clone)]
pub struct TellLog {
    /// (`sender_name`, `when_received`). Capped at `cap`; oldest dropped.
    pub entries: std::collections::VecDeque<(String, std::time::Instant)>,
    pub cap: usize,
}

impl TellLog {
    #[must_use]
    pub const fn with_cap(cap: usize) -> Self {
        Self {
            entries: std::collections::VecDeque::new(),
            cap,
        }
    }

    pub fn push(&mut self, name: String) {
        self.entries.push_front((name, std::time::Instant::now()));
        while self.entries.len() > self.cap {
            self.entries.pop_back();
        }
    }
}

/// Wall-clock instant of the player's most recent dispatched command.
/// Stamped at the top of `commands::dispatch`. Absent on a player who has
/// connected but not yet typed anything — `who` shows them as fresh.
#[derive(Component, Debug, Clone, Copy)]
pub struct LastInputAt(pub std::time::Instant);

/// Wall-clock instant the player's character entity was spawned (== when
/// they finished login and entered the world). Stamped in
/// `login::spawn_player`; persists for the duration of the session.
#[derive(Component, Debug, Clone, Copy)]
pub struct LoggedInAt(pub std::time::Instant);

/// Marker linking a spawned mob entity back to the `MobResets.id` row
/// that produced it. The respawn tick system queries entities by this
/// component to count live instances per reset and decide whether to
/// refill below `max_instances`.
#[derive(Component, Debug, Clone, Copy)]
pub struct FromMobReset(pub i32);

/// Marker linking a spawned item entity back to the `ObjectResets.id`
/// row that produced it. Each reset row owns at most one live instance
/// at a time; the respawn tick re-fires a row only when its instance no
/// longer exists, so multiple rows in the same room legitimately stack
/// (a basket of apples, a rose bush). `max_instances` on the row is the
/// global ceiling across all rows for that proto.
#[derive(Component, Debug, Clone, Copy)]
pub struct FromObjectReset(pub i32);

/// Player-set epithet shown after the character name on `who` and
/// (eventually) on the long room-occupant line. Loaded from
/// `Characters.title` at login; absent when the column is NULL or
/// empty. Mutable via the `title` command (setter to land in a
/// follow-up commit).
#[derive(Component, Debug, Clone)]
pub struct Title(pub String);

/// Character identity beyond name: class, race, experience. Loaded
/// from `Characters` at login. Class and race are display-only for
/// now; once the casting pipeline grows formula evaluation, the same
/// component will supply the input for `level` / `class.bonus_*` /
/// race-modifier expressions.
#[derive(Component, Debug, Clone)]
pub struct Profile {
    pub level: i32,
    /// FK to `Class.id`. None for characters who haven't picked a class.
    pub class_id: Option<i32>,
    /// Schema enum value as a raw string — runtime renders it directly.
    pub race: String,
    pub experience: i32,
    /// Schema column verbatim — typically "male" / "female" /
    /// "neutral". Triggers gate on this via `actor.gender` for
    /// gendered room/dialogue logic.
    pub gender: String,
}

/// One prepared spell slot in a player's memorize list. `ready`
/// flips to true once `prep_secs_remaining` decrements to 0 (the
/// rest-tick handles that under Sleeping/Resting/Sitting postures).
/// `cast` consumes only `ready=true` entries.
#[derive(Debug, Clone, Copy)]
pub struct MemEntry {
    pub ability_id: i32,
    pub circle: i32,
    pub ready: bool,
    pub prep_secs_remaining: i32,
}

/// Session-only spell memorization list — one entry per memorized
/// instance. Caster classes (Sorcerer/Cleric/etc.) populate this
/// via `memorize <spell>`; `cast` gates on `ready=true`. Not
/// persisted across disconnect (no schema column today); v1
/// expects players to re-memorize on reconnect.
#[derive(Component, Debug, Clone, Default)]
pub struct MemorizedSpells {
    /// Order matters: `forget <name>` removes the first matching
    /// entry (preferring not-yet-ready), mirroring `FieryMUD`'s
    /// stack-of-prepared-spells semantics.
    pub entries: Vec<MemEntry>,
}

impl MemorizedSpells {
    /// Count how many slots in `circle` are currently in use
    /// (whether or not they're ready). Used by the `slots`
    /// readout's `used / max` line.
    #[must_use]
    pub fn used_in_circle(&self, circle: i32) -> i32 {
        i32::try_from(self.entries.iter().filter(|e| e.circle == circle).count())
            .unwrap_or(0)
    }

    /// Count how many entries are ready (memorized) in `circle`.
    #[must_use]
    pub fn ready_in_circle(&self, circle: i32) -> i32 {
        i32::try_from(
            self.entries
                .iter()
                .filter(|e| e.circle == circle && e.ready)
                .count(),
        )
        .unwrap_or(0)
    }
}

/// What abilities (spells / chants / songs / skills) a character has
/// learned, plus their proficiency. Loaded from `CharacterAbilities`
/// at login. The `known` flag mirrors the schema column — `true` for
/// fully-learned abilities, `false` for ones the character has been
/// taught but hasn't yet mastered. The `spells` listing filters on
/// this; `cast` will eventually gate on it once real casting lands.
#[derive(Component, Debug, Clone, Default)]
pub struct KnownAbilities {
    /// (`ability_id`, proficiency 0–1000+, known flag). Sorted by
    /// `ability_id` (matches the DB's ORDER BY).
    pub entries: Vec<(i32, i32, bool)>,
}

impl KnownAbilities {
    /// True if the character has any (even unknown-but-being-learned) row
    /// for this ability — used to filter the `spells` listing.
    #[must_use]
    pub fn has_any(&self, ability_id: i32) -> bool {
        self.entries.iter().any(|(id, _, _)| *id == ability_id)
    }
}

/// Per-character command aliases. Each entry is `(alias, command)`
/// — the dispatcher rewrites `<alias> [args]` to `<command> [args]`
/// before lookup. Persisted to `CharacterAliases`. Sorted by alias
/// at load to match the table's ORDER BY.
#[derive(Component, Debug, Clone, Default)]
pub struct Aliases {
    pub entries: Vec<(String, String)>,
}

impl Aliases {
    /// Look up the expansion for `alias`, case-insensitive.
    #[must_use]
    pub fn get(&self, alias: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(a, _)| a.eq_ignore_ascii_case(alias))
            .map(|(_, c)| c.as_str())
    }

    /// Insert or replace an alias. Returns true if a previous entry
    /// with the same alias was overwritten.
    pub fn set(&mut self, alias: &str, command: String) -> bool {
        let existing = self
            .entries
            .iter_mut()
            .find(|(a, _)| a.eq_ignore_ascii_case(alias));
        if let Some(entry) = existing {
            entry.1 = command;
            return true;
        }
        self.entries.push((alias.to_string(), command));
        self.entries.sort_by(|a, b| a.0.cmp(&b.0));
        false
    }

    /// Remove an alias. Returns true if it existed.
    pub fn remove(&mut self, alias: &str) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|(a, _)| !a.eq_ignore_ascii_case(alias));
        before != self.entries.len()
    }
}

/// Admin sanction marker: the player's command dispatch is refused
/// (with a message) until the marker is removed. Session-scoped — does
/// not persist across disconnect/reconnect.
#[derive(Component, Debug, Clone, Copy)]
pub struct Frozen;

/// Output verbosity preference for info commands (score/equipment/look/etc).
/// Session-scoped for now — eventual home is an Account field or a
/// `PlayerToggle` row so it persists across logins.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UiStyle {
    /// ASCII-art borders, generous spacing — for capable terminals.
    Fancy,
    /// The current default: clean, indented, one fact per line.
    #[default]
    Standard,
    /// Single-line dense readout — for narrow viewports or scripting.
    Minimal,
}

impl UiStyle {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Fancy => "fancy",
            Self::Standard => "standard",
            Self::Minimal => "minimal",
        }
    }

    /// Parse a user-typed style word; case-insensitive, with sensible aliases.
    #[must_use]
    pub fn from_label(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "fancy" => Some(Self::Fancy),
            "standard" | "default" | "normal" => Some(Self::Standard),
            "minimal" | "tight" | "brief" => Some(Self::Minimal),
            _ => None,
        }
    }
}

/// Per-player prompt template loaded from `Characters.prompt`. Variable
/// substitution: `%h`/`%H` (current/max HP). More variables (`%v`/`%V`
/// for stamina, `%n` for name, etc.) land when the systems they reference
/// do.
#[derive(Component, Debug, Clone)]
pub struct Prompt(pub String);

/// Where a player goes when they `recall`. Resolved from
/// `Characters.recall_room_*` at spawn time. Absent if the row had NULLs
/// for either coordinate or the target room isn't loaded.
#[derive(Component, Debug, Clone, Copy)]
pub struct RecallPoint(pub Entity);

/// Toggleable per-player preferences (DEAF, `NO_TELL`, AFK, etc.). Loaded
/// from `Characters.player_flags` on login; saved back on disconnect.
#[derive(Component, Debug, Clone, Default)]
pub struct PlayerFlags(pub Vec<mud_db::enums::PlayerFlag>);

impl PlayerFlags {
    #[must_use]
    pub fn has(&self, flag: mud_db::enums::PlayerFlag) -> bool {
        self.0.contains(&flag)
    }

    /// Toggle the flag and return whether it ended up on (true) or off (false).
    pub fn toggle(&mut self, flag: mud_db::enums::PlayerFlag) -> bool {
        if let Some(idx) = self.0.iter().position(|f| *f == flag) {
            self.0.swap_remove(idx);
            false
        } else {
            self.0.push(flag);
            true
        }
    }
}

/// Body posture. The schema's Position enum is broader (DEAD, GHOST,
/// `MORTALLY_WOUNDED`, INCAPACITATED, STUNNED) — those land when combat
/// has real damage states, not posture changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostureKind {
    Standing,
    Sitting,
    Kneeling,
    Resting,
    Sleeping,
}

impl PostureKind {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Standing => "standing",
            Self::Sitting => "sitting",
            Self::Kneeling => "kneeling",
            Self::Resting => "resting",
            Self::Sleeping => "sleeping",
        }
    }

    /// Posture rank matching the schema's `Position` enum sort order
    /// (SLEEPING=6 .. STANDING=9). Used by ability gating: `cast`
    /// refuses if `current_rank < ability.min_posture_rank`. The
    /// schema's lower ranks (DEAD .. STUNNED) aren't modeled here, so
    /// any rank ≤ 6 passes for every runtime posture. `Kneeling`
    /// shares rank 8 with `Sitting` — most ability gates that allow
    /// sitting allow kneeling, and vice versa.
    #[must_use]
    pub fn rank(self) -> i32 {
        match self {
            Self::Sleeping => 6,
            Self::Resting => 7,
            Self::Sitting | Self::Kneeling => 8,
            Self::Standing => 9,
        }
    }
}

#[derive(Component, Debug, Clone, Copy)]
pub struct Posture(pub PostureKind);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectSource {
    Spell,
    Item,
    Room,
    Admin,
    Other(String),
}

/// One active effect. Each application is its own child entity, attached
/// via `AppliedTo`. Cap on stacking is per-effect-type and lives in code
/// that applies effects (not enforced here).
#[derive(Component, Debug, Clone)]
pub struct EffectInstance {
    /// FK into the `EffectCatalog` resource.
    pub kind: i32,
    /// Cached display name (also in catalog; copied here so messages don't
    /// need to look up the catalog every tick).
    pub name: String,
    pub strength: i32,
    /// Seconds remaining; -1 means permanent.
    pub remaining_secs: i32,
    pub source: EffectSource,
    /// `Ability.id` of whatever spawned this effect, when known. Used
    /// by `effects_tick` to look up `wearoff_to_target` /
    /// `wearoff_to_room` templates in `AbilityCatalog.messages`. None
    /// for hardcoded effects (bleed-on-rend, admin tests) where there
    /// is no originating ability row.
    pub ability_id: Option<i32>,
}

/// Edge from an `EffectInstance` entity back to the entity that's affected.
#[derive(Component, Debug, Clone, Copy)]
pub struct AppliedTo(pub Entity);

/// Per-effect "what stat did I bump and by how much" companion record
/// for `modify`-effect-type `EffectInstance`s. Lives on the effect
/// entity alongside `EffectInstance` and `AppliedTo`. Used by the
/// effects tick to subtract the same delta back when the effect
/// fades — keeps stacking buffs from each other's expiries cleanly.
#[derive(Component, Debug, Clone)]
pub struct ModifyDelta {
    pub target: String,
    pub amount: i32,
}

/// Composite (zone, id) identity for entities loaded from the schema.
/// Lets the runtime round-trip an entity back to its DB row.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorldKey {
    pub zone: i32,
    pub id: i32,
}

#[derive(Component, Debug, Clone)]
pub struct Named {
    pub name: String,
}

/// Long-form description text. Used by `look` for rooms, by `examine` for
/// items/mobs eventually. Empty string means "no description".
#[derive(Component, Debug, Clone)]
pub struct Description(pub String);

/// Containment relationship: this entity is "inside" the target.
/// Rooms are inside their Zone; later, players/objects will be inside Rooms.
#[derive(Component, Debug, Clone, Copy)]
pub struct Located(pub Entity);

#[derive(Component, Debug, Clone, Copy)]
pub struct RoomSector(pub Sector);

#[derive(Debug, Clone, Copy)]
pub struct ExitData {
    /// Resolved target room entity, if the target exists in the loaded world.
    pub to: Option<Entity>,
    pub state: ExitState,
    /// Composite (zone, id) of the required key Object. None for
    /// exits that need no key. `unlock` looks up the player's
    /// inventory by exact `WorldKey` match.
    pub key: Option<(i32, i32)>,
}

#[derive(Component, Debug, Clone, Default)]
pub struct Exits(pub HashMap<Direction, ExitData>);

#[cfg(test)]
mod tests {
    use super::*;
    use mud_db::enums::PlayerFlag;

    #[test]
    fn player_flags_toggle_round_trip() {
        let mut pf = PlayerFlags::default();
        assert!(!pf.has(PlayerFlag::Afk));
        // First toggle adds, returns true (now on).
        assert!(pf.toggle(PlayerFlag::Afk));
        assert!(pf.has(PlayerFlag::Afk));
        // Second toggle removes, returns false (now off).
        assert!(!pf.toggle(PlayerFlag::Afk));
        assert!(!pf.has(PlayerFlag::Afk));
    }

    #[test]
    fn player_flags_toggle_independent_per_flag() {
        let mut pf = PlayerFlags::default();
        pf.toggle(PlayerFlag::Afk);
        pf.toggle(PlayerFlag::Deaf);
        pf.toggle(PlayerFlag::NoTell);
        assert!(pf.has(PlayerFlag::Afk));
        assert!(pf.has(PlayerFlag::Deaf));
        assert!(pf.has(PlayerFlag::NoTell));
        assert!(!pf.has(PlayerFlag::Wimpy));
        // Toggling one off doesn't affect the others.
        pf.toggle(PlayerFlag::Deaf);
        assert!(pf.has(PlayerFlag::Afk));
        assert!(!pf.has(PlayerFlag::Deaf));
        assert!(pf.has(PlayerFlag::NoTell));
    }

    #[test]
    fn player_flags_has_on_empty() {
        let pf = PlayerFlags::default();
        // No flags → has() returns false for everything.
        assert!(!pf.has(PlayerFlag::Afk));
        assert!(!pf.has(PlayerFlag::Brief));
        assert!(!pf.has(PlayerFlag::Wimpy));
    }

    #[test]
    fn slot_round_trip_db_label() {
        for s in Slot::ORDER {
            let label = s.db_label();
            assert_eq!(Slot::from_label(label), Some(*s), "round-trip {label}");
        }
    }

    #[test]
    fn slot_from_label_is_case_insensitive() {
        assert_eq!(Slot::from_label("body"), Some(Slot::Body));
        assert_eq!(Slot::from_label("Body"), Some(Slot::Body));
        assert_eq!(Slot::from_label("BODY"), Some(Slot::Body));
    }

    #[test]
    fn slot_from_label_collapses_neck_pair() {
        assert_eq!(Slot::from_label("NECK_1"), Some(Slot::Neck));
        assert_eq!(Slot::from_label("NECK_2"), Some(Slot::Neck));
        assert_eq!(Slot::from_label("NECK"), Some(Slot::Neck));
    }

    #[test]
    fn slot_from_label_extended_coverage() {
        // Slots added 2026-05-02 covering ~340 previously-unwearable
        // imported items (cloaks/wristbands/goggles/masks/badges/etc.).
        assert_eq!(Slot::from_label("BADGE"), Some(Slot::Badge));
        assert_eq!(Slot::from_label("ABOUT"), Some(Slot::About));
        assert_eq!(Slot::from_label("EARS"), Some(Slot::Ears));
        // Schema enum is `EAR` (singular); from_label accepts both
        // forms so legacy DB rows that use either spelling resolve.
        assert_eq!(Slot::from_label("EAR"), Some(Slot::Ears));
        assert_eq!(Slot::from_label("WRIST"), Some(Slot::Wrist));
        assert_eq!(Slot::from_label("EYES"), Some(Slot::Eyes));
        assert_eq!(Slot::from_label("FACE"), Some(Slot::Face));
        assert_eq!(Slot::from_label("HOVER"), Some(Slot::Hover));
        assert_eq!(Slot::from_label(""), None);
        assert_eq!(Slot::from_label("garbage"), None);
    }
}
