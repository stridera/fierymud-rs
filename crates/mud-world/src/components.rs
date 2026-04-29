use std::collections::HashMap;

use bevy_ecs::prelude::*;
use mud_db::enums::{Direction, ExitState, Sector};

/// Marker: this entity is a zone (organizational grouping of rooms).
#[derive(Component, Debug, Clone, Copy)]
pub struct Zone;

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

/// Marker: this entity is an item instance (weapon, potion, container, …).
#[derive(Component, Debug, Clone, Copy)]
pub struct Item;

/// Lookup keywords for an entity, used by `get`/`drop`/`attack` matching.
/// First entry is typically the canonical identifier ("sword"), followed by
/// other words a player might type ("rusty", "iron").
#[derive(Component, Debug, Clone)]
pub struct Keywords(pub Vec<String>);

/// Equipment slots a wearable item can occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Head,
    Neck,
    Body,
    Arms,
    Hands,
    LeftFinger,
    RightFinger,
    Waist,
    Legs,
    Feet,
    Wield,
    Hold,
    Light,
}

impl Slot {
    pub const ORDER: &'static [Self] = &[
        Self::Head,
        Self::Neck,
        Self::Body,
        Self::Arms,
        Self::Hands,
        Self::LeftFinger,
        Self::RightFinger,
        Self::Waist,
        Self::Legs,
        Self::Feet,
        Self::Wield,
        Self::Hold,
        Self::Light,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Neck => "neck",
            Self::Body => "body",
            Self::Arms => "arms",
            Self::Hands => "hands",
            Self::LeftFinger => "finger (left)",
            Self::RightFinger => "finger (right)",
            Self::Waist => "waist",
            Self::Legs => "legs",
            Self::Feet => "feet",
            Self::Wield => "wielded",
            Self::Hold => "held",
            Self::Light => "light",
        }
    }

    /// Canonical `SCREAMING_SNAKE_CASE` name for DB persistence — matches
    /// the `equipped_location` text we write to `CharacterItems`.
    #[must_use]
    pub fn db_label(self) -> &'static str {
        match self {
            Self::Head => "HEAD",
            Self::Neck => "NECK",
            Self::Body => "BODY",
            Self::Arms => "ARMS",
            Self::Hands => "HANDS",
            Self::LeftFinger => "FINGER_LEFT",
            Self::RightFinger => "FINGER_RIGHT",
            Self::Waist => "WAIST",
            Self::Legs => "LEGS",
            Self::Feet => "FEET",
            Self::Wield => "WIELD",
            Self::Hold => "HOLD",
            Self::Light => "LIGHT",
        }
    }

    /// Parse the free-text `equipped_location` from `CharacterItems`. Case
    /// insensitive. Returns None for slots we don't yet model (BADGE,
    /// EARS, FACE, ABOUT, NECK_1/NECK_2-as-distinct, etc.) — the caller
    /// treats those rows as inventory rather than dropping them entirely.
    #[must_use]
    pub fn from_label(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "HEAD" => Some(Self::Head),
            // Schema sometimes splits NECK_1/NECK_2 — we collapse to one
            // slot for now since the runtime doesn't model the pair.
            "NECK" | "NECK_1" | "NECK_2" => Some(Self::Neck),
            "BODY" => Some(Self::Body),
            "ARMS" => Some(Self::Arms),
            "HANDS" => Some(Self::Hands),
            "FINGER_LEFT" | "FINGER_R" => Some(Self::LeftFinger),
            "FINGER_RIGHT" | "FINGER_L" => Some(Self::RightFinger),
            "WAIST" => Some(Self::Waist),
            "LEGS" => Some(Self::Legs),
            "FEET" => Some(Self::Feet),
            "WIELD" => Some(Self::Wield),
            "HOLD" => Some(Self::Hold),
            "LIGHT" => Some(Self::Light),
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

#[derive(Component, Debug, Clone, Copy, Default)]
pub struct CombatStats {
    pub hit_roll: i32,
    pub dmg_roll: i32,
    pub ac: i32,
    pub alignment: i32,
}

/// Combat state: this entity is currently fighting the target. Removed when
/// combat ends (death, flee, room mismatch).
#[derive(Component, Debug, Clone, Copy)]
pub struct Fighting(pub Entity);

/// Following state: this entity moves automatically when the target moves.
/// Distinct from Located — followers retain their own Located even while
/// chasing the leader, and the follow edge is preserved across rooms.
#[derive(Component, Debug, Clone, Copy)]
pub struct Follower(pub Entity);

/// Most recent sender of a `tell` to this entity. Used by `reply` to find
/// the previous correspondent. Cleared (or stale-checked) on the receiver
/// side, not the sender side.
#[derive(Component, Debug, Clone, Copy)]
pub struct LastTeller(pub Entity);

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
    Resting,
    Sleeping,
}

impl PostureKind {
    #[must_use] 
    pub fn label(self) -> &'static str {
        match self {
            Self::Standing => "standing",
            Self::Sitting => "sitting",
            Self::Resting => "resting",
            Self::Sleeping => "sleeping",
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
}

/// Edge from an `EffectInstance` entity back to the entity that's affected.
#[derive(Component, Debug, Clone, Copy)]
pub struct AppliedTo(pub Entity);

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
    fn slot_from_label_unknowns_return_none() {
        assert_eq!(Slot::from_label("BADGE"), None);
        assert_eq!(Slot::from_label("ABOUT"), None);
        assert_eq!(Slot::from_label("EARS"), None);
        assert_eq!(Slot::from_label(""), None);
        assert_eq!(Slot::from_label("garbage"), None);
    }
}
