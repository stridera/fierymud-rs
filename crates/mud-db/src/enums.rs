use serde::{Deserialize, Serialize};

/// Player feedback categories — drives `reports.report_type` on the
/// in-game `idea` / `bug` / `typo` commands. Each row carries a
/// reporter, room, and free-text message; staff triages via the
/// open/in-progress/closed status field.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "ReportType", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReportType {
    Bug,
    Idea,
    Typo,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "ResetMode", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResetMode {
    Never,
    Empty,
    Normal,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "Hemisphere", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Hemisphere {
    Northwest,
    Northeast,
    Southwest,
    Southeast,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "Climate", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Climate {
    None,
    Semiarid,
    Arid,
    Oceanic,
    Temperate,
    Subtropical,
    Tropical,
    Subarctic,
    Arctic,
    Alpine,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "Sector", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Sector {
    Structure,
    City,
    Field,
    Forest,
    Hills,
    Mountain,
    Shallows,
    Water,
    Underwater,
    Air,
    Road,
    Grasslands,
    Cave,
    Ruins,
    Swamp,
    Beach,
    Underdark,
    Astralplane,
    Airplane,
    Fireplane,
    Earthplane,
    Etherealplane,
    Avernus,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = "Direction", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Direction {
    North,
    East,
    South,
    West,
    Up,
    Down,
    Northeast,
    Northwest,
    Southeast,
    Southwest,
    In,
    Out,
    Portal,
    None,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "ExitState", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExitState {
    Open,
    Closed,
    Locked,
}

/// Boolean attributes layered on a `RoomExit`. Stored as a Postgres
/// `_ExitFlag` array on the row. Most flags only matter once the
/// corresponding gameplay system lands; `Hidden` is the first one
/// the runtime cares about (filtered from default exit lists, only
/// revealed by `search`).
///
/// `no_pg_array` disables sqlx's auto-derived `PgHasArrayType`
/// impl so the manual impl below can supply the case-preserving
/// quoted array name (`_ExitFlag`); the auto-derive lowercases.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = "ExitFlag", rename_all = "SCREAMING_SNAKE_CASE", no_pg_array)]
pub enum ExitFlag {
    IsDoor,
    Pickproof,
    Hidden,
    Bashable,
    Magicproof,
}

impl sqlx::postgres::PgHasArrayType for ExitFlag {
    fn array_type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name(r#""_ExitFlag""#)
    }
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "ObjectType", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ObjectType {
    Nothing,
    Light,
    Scroll,
    Wand,
    Staff,
    Weapon,
    Fireweapon,
    Missile,
    Treasure,
    Armor,
    Potion,
    Worn,
    Other,
    Trash,
    Trap,
    Container,
    Note,
    Drinkcontainer,
    Key,
    Food,
    Money,
    Pen,
    Boat,
    Fountain,
    Portal,
    Rope,
    Spellbook,
    Wall,
    Touchstone,
    Board,
    Instrument,
    Vehicle,
    Corpse,
    Kit,
    Wings,
    Perfume,
    Disguise,
    Poison,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "AchievementCategory", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AchievementCategory {
    Combat,
    Exploration,
    Social,
    Crafting,
    Misc,
}

impl AchievementCategory {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Combat => "Combat",
            Self::Exploration => "Exploration",
            Self::Social => "Social",
            Self::Crafting => "Crafting",
            Self::Misc => "Misc",
        }
    }
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "MobRole", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MobRole {
    Trash,
    Normal,
    Elite,
    Miniboss,
    Boss,
    RaidBoss,
}

/// Special NPC service roles. A mob can carry multiple
/// professions (banker + receptionist, etc). Mirrors the schema
/// enum verbatim.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "MobProfession", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MobProfession {
    Banker,
    Shopkeeper,
    Receptionist,
    Postmaster,
    Guildmaster,
    Trainer,
}

/// Three-bucket alignment used for item restrictions. The
/// schema's `Objects.restricted_alignments` column is an array
/// of these. The runtime compares against the killer's i32
/// alignment via `Alignment::from_score`.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "Alignment", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Alignment {
    Good,
    Neutral,
    Evil,
}

impl Alignment {
    /// Map an integer alignment score onto the three-bucket enum.
    /// Mirrors classic `FieryMUD`: ≥350 = Good, ≤-350 = Evil, else
    /// Neutral. The thresholds are the same ones the schema's
    /// `protectedKind`/race-restriction docs reference.
    #[must_use]
    pub const fn from_score(score: i32) -> Self {
        if score >= 350 {
            Self::Good
        } else if score <= -350 {
            Self::Evil
        } else {
            Self::Neutral
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Good => "good",
            Self::Neutral => "neutral",
            Self::Evil => "evil",
        }
    }
}

/// "Kill the wrong target" alignment-penalty marker on mobs.
/// `Normal` is the default and incurs no penalty; the others
/// scale a player's alignment toward EVIL on kill.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[sqlx(type_name = "ProtectedKind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProtectedKind {
    #[default]
    Normal,
    Innocent,
    Shopkeeper,
    QuestNpc,
}

impl ProtectedKind {
    /// Alignment delta applied to the killer when this mob dies.
    /// Negative numbers shift toward EVIL. `Normal` is 0 (no
    /// penalty). Tunable; values picked to make `Innocent` a
    /// strong shaping signal without one-shotting alignment.
    #[must_use]
    pub const fn alignment_penalty(self) -> i32 {
        match self {
            Self::Normal => 0,
            Self::Innocent => -250,
            Self::Shopkeeper => -150,
            Self::QuestNpc => -75,
        }
    }
}

/// Per-mob behavior flags, sourced from `Mobs.behaviors`. Mirrors
/// the schema's `MobBehavior` enum verbatim. Variants the runtime
/// doesn't read yet stay in the spawn-time list; future systems
/// can light them up without re-running the DB query.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "MobBehavior", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MobBehavior {
    Sentinel,
    StayZone,
    Scavenger,
    Track,
    SlowTrack,
    FastTrack,
    Wimpy,
    Aware,
    Helper,
    Protector,
    Peacekeeper,
    NoBash,
    NoSummon,
    NoVicious,
    Memory,
    Teacher,
    Meditate,
    NoScript,
    NoClassAi,
    Peaceful,
    NoKill,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = "UserRole", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UserRole {
    Player,
    Immortal,
    Builder,
    HeadBuilder,
    Coder,
    Implementor,
}

impl UserRole {
    /// Numeric hierarchy used for `min_role` checks. Higher = more privileged.
    /// Roles aren't strictly linear in MUDs (Coder vs `HeadBuilder` etc.), so we
    /// expose `rank()` rather than deriving Ord on the enum.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Player => 0,
            Self::Immortal => 100,
            Self::Builder => 110,
            Self::HeadBuilder => 120,
            Self::Coder => 130,
            Self::Implementor => 140,
        }
    }

    #[must_use]
    pub const fn at_least(self, min: Self) -> bool {
        self.rank() >= min.rank()
    }

    /// Display label used by `account` and the role readout.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Player => "Player",
            Self::Immortal => "Immortal",
            Self::Builder => "Builder",
            Self::HeadBuilder => "Head Builder",
            Self::Coder => "Coder",
            Self::Implementor => "Implementor",
        }
    }
}

/// Wear-slot flags from the schema. Loaded read-only with the
/// `wearFlags` column; `WearableIn` derives a single primary slot
/// from the first relevant flag. The runtime maps subsets of these
/// onto our smaller `PostureKind`-shaped slot enum.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = "WearFlag", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WearFlag {
    Finger,
    Neck,
    Ear,
    Wrist,
    Head,
    Eyes,
    Face,
    Body,
    About,
    Arms,
    Hands,
    Waist,
    Belt,
    Legs,
    Feet,
    Tail,
    Mainhand,
    Offhand,
    Twohand,
    Badge,
    Hover,
    Disguise,
}

// `type_name` carries embedded quotes so sqlx preserves the mixed-case
// identifier through Postgres's name lookup; without quotes the
// element name `PlayerFlag` is folded to lowercase by the server and
// the encode path fails with "type playerflag does not exist".
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = r#""PlayerFlag""#, rename_all = "SCREAMING_SNAKE_CASE")]
#[sqlx(no_pg_array)]
pub enum PlayerFlag {
    Brief,
    Compact,
    NoRepeat,
    AutoLoot,
    AutoGold,
    AutoSplit,
    AutoExit,
    AutoAssist,
    Wimpy,
    ShowDiceRolls,
    Afk,
    Deaf,
    NoTell,
    NoSummon,
    Quest,
    PkEnabled,
    Consent,
    ColorBlind,
    Msp,
    MxpEnabled,
    HolyLight,
    ShowIds,
}

// sqlx's auto-derived PgHasArrayType lowercases the array type name
// (sends `_playerflag`), but our Postgres array type is mixed-case
// `_PlayerFlag`. Quote-wrapping the static literal preserves the case
// through sqlx's pg_type lookup — the embedded `"` becomes a quoted
// identifier in the lookup SQL.
impl sqlx::postgres::PgHasArrayType for PlayerFlag {
    fn array_type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name(r#""_PlayerFlag""#)
    }
}

impl PlayerFlag {
    /// Case-insensitive parse from the canonical `SCREAMING_SNAKE_CASE` name.
    /// Used by the `toggle` command.
    #[must_use]
    pub fn from_label(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "BRIEF" => Some(Self::Brief),
            "COMPACT" => Some(Self::Compact),
            "NO_REPEAT" | "NOREPEAT" => Some(Self::NoRepeat),
            "AUTO_LOOT" | "AUTOLOOT" => Some(Self::AutoLoot),
            "AUTO_GOLD" | "AUTOGOLD" => Some(Self::AutoGold),
            "AUTO_SPLIT" | "AUTOSPLIT" => Some(Self::AutoSplit),
            "AUTO_EXIT" | "AUTOEXIT" => Some(Self::AutoExit),
            "AUTO_ASSIST" | "AUTOASSIST" => Some(Self::AutoAssist),
            "WIMPY" => Some(Self::Wimpy),
            "SHOW_DICE_ROLLS" | "DICE" => Some(Self::ShowDiceRolls),
            "AFK" => Some(Self::Afk),
            "DEAF" => Some(Self::Deaf),
            "NO_TELL" | "NOTELL" => Some(Self::NoTell),
            "NO_SUMMON" | "NOSUMMON" => Some(Self::NoSummon),
            "QUEST" => Some(Self::Quest),
            "PK_ENABLED" | "PK" => Some(Self::PkEnabled),
            "CONSENT" => Some(Self::Consent),
            "COLOR_BLIND" | "COLORBLIND" => Some(Self::ColorBlind),
            "MSP" => Some(Self::Msp),
            "MXP_ENABLED" | "MXP" => Some(Self::MxpEnabled),
            "HOLY_LIGHT" | "HOLYLIGHT" => Some(Self::HolyLight),
            "SHOW_IDS" | "SHOWIDS" => Some(Self::ShowIds),
            _ => None,
        }
    }

    /// Flags that should only ever be toggled by Builder+ accounts.
    /// `cmd_toggle` consults this so the generic-toggle path can't
    /// be used to bypass the dedicated god-toggle commands.
    #[must_use]
    pub fn is_god_only(self) -> bool {
        matches!(self, Self::HolyLight | Self::ShowIds)
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Brief => "BRIEF",
            Self::Compact => "COMPACT",
            Self::NoRepeat => "NO_REPEAT",
            Self::AutoLoot => "AUTO_LOOT",
            Self::AutoGold => "AUTO_GOLD",
            Self::AutoSplit => "AUTO_SPLIT",
            Self::AutoExit => "AUTO_EXIT",
            Self::AutoAssist => "AUTO_ASSIST",
            Self::Wimpy => "WIMPY",
            Self::ShowDiceRolls => "SHOW_DICE_ROLLS",
            Self::Afk => "AFK",
            Self::Deaf => "DEAF",
            Self::NoTell => "NO_TELL",
            Self::NoSummon => "NO_SUMMON",
            Self::Quest => "QUEST",
            Self::PkEnabled => "PK_ENABLED",
            Self::Consent => "CONSENT",
            Self::ColorBlind => "COLOR_BLIND",
            Self::Msp => "MSP",
            Self::MxpEnabled => "MXP_ENABLED",
            Self::HolyLight => "HOLY_LIGHT",
            Self::ShowIds => "SHOW_IDS",
        }
    }
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = "Permission", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Permission {
    Build,
    Code,
    Admin,
    God,
    Shutdown,
    Wizlock,
    Syslog,
    Log,
    Force,
    Snoop,
    Freeze,
    Thaw,
    Ban,
    Unban,
    Dc,
    Advance,
    Restore,
    Notitle,
    Squelch,
    Teleport,
    Transfer,
    Summon,
    Invisible,
    Nohassle,
    ZoneReset,
    Wiznet,
    /// Legacy in-game OLC. Not used in fierymud-rs — world editing is done
    /// through Muditor. Kept here only so sqlx can decode rows that still
    /// have this Postgres enum value granted.
    Olc,
}
