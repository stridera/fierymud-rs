use serde::{Deserialize, Serialize};

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
#[sqlx(type_name = "MobRole", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MobRole {
    Trash,
    Normal,
    Elite,
    Miniboss,
    Boss,
    RaidBoss,
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
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = "PlayerFlag", rename_all = "SCREAMING_SNAKE_CASE")]
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
