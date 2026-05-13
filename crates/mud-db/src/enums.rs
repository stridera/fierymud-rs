// IMPORTANT: every `#[sqlx(type_name = ...)]` attribute on a custom
// Postgres enum below uses the embedded-quote form `r#""TypeName""#`
// rather than the bare `"TypeName"` you'd expect. Reason: Prisma
// generates these types case-preserved (`CREATE TYPE "Position"
// AS ENUM ...`), and sqlx 0.8 resolves a custom type's OID at encode
// time via `SELECT $1::regtype::oid`. With an unquoted value,
// Postgres folds the identifier to lowercase inside the regtype cast
// and the lookup fails at runtime with `syntax error at or near
// "<Name>"`. The macro-time `query!` prepare check does NOT exercise
// that regtype path, so the bug only surfaces when a parameter is
// actually bound on a live query.
//
// When adding a new mixed-case Prisma-generated PG enum here, copy
// the embedded-quote pattern. Lowercase / all-caps PG types don't
// need it, but applying it uniformly is harmless and keeps the
// convention skim-readable.

use serde::{Deserialize, Serialize};

/// Player feedback categories — drives `reports.report_type` on the
/// in-game `idea` / `bug` / `typo` commands. Each row carries a
/// reporter, room, and free-text message; staff triages via the
/// open/in-progress/closed status field.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = r#""ReportType""#, rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReportType {
    Bug,
    Idea,
    Typo,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = r#""ResetMode""#, rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResetMode {
    Never,
    Empty,
    Normal,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = r#""Hemisphere""#, rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Hemisphere {
    Northwest,
    Northeast,
    Southwest,
    Southeast,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = r#""Climate""#, rename_all = "SCREAMING_SNAKE_CASE")]
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
#[sqlx(type_name = r#""Sector""#, rename_all = "SCREAMING_SNAKE_CASE")]
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

/// Body / life-state position. Mirrors the schema's `Position` enum
/// (Prisma model). The runtime maps the active subset
/// (`STANDING`/`SITTING`/`RESTING`/`SLEEPING`) onto `PostureKind` and
/// the dead-but-incorporeal `GHOST` value onto a runtime marker; the
/// remaining values (`DEAD`/`MORTALLY_WOUNDED`/`INCAPACITATED`/`STUNNED`)
/// aren't yet modeled and round-trip as `STANDING` on load.
// `type_name` carries embedded quotes because sqlx 0.8 looks the
// type's OID up at encode time via `SELECT $1::regtype::oid`. PG
// folds the unquoted form to lowercase and the cast fails with
// `syntax error at or near "Position"`. The macro-time prepare
// doesn't exercise this path, so the bug only surfaces at runtime.
// Required for every Prisma-generated mixed-case enum type.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = r#""Position""#, rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Position {
    Dead,
    Ghost,
    MortallyWounded,
    Incapacitated,
    Stunned,
    Sleeping,
    Resting,
    Sitting,
    Standing,
}

impl Position {
    /// Schema-text form for the wire — kept stable so save sites can
    /// round-trip without re-deriving the macro mapping.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dead => "DEAD",
            Self::Ghost => "GHOST",
            Self::MortallyWounded => "MORTALLY_WOUNDED",
            Self::Incapacitated => "INCAPACITATED",
            Self::Stunned => "STUNNED",
            Self::Sleeping => "SLEEPING",
            Self::Resting => "RESTING",
            Self::Sitting => "SITTING",
            Self::Standing => "STANDING",
        }
    }
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = r#""Direction""#, rename_all = "SCREAMING_SNAKE_CASE")]
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
#[sqlx(type_name = r#""ExitState""#, rename_all = "SCREAMING_SNAKE_CASE")]
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
#[sqlx(type_name = r#""ExitFlag""#, rename_all = "SCREAMING_SNAKE_CASE", no_pg_array)]
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
#[sqlx(type_name = r#""ObjectType""#, rename_all = "SCREAMING_SNAKE_CASE")]
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

impl ObjectType {
    /// Human-readable label for `identify` / `vitem` / proto readouts.
    /// Debug-derived names like "Drinkcontainer" / "Fireweapon" are
    /// jarring on the screen; this expands them to the form players
    /// and builders actually use.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Nothing => "Nothing",
            Self::Light => "Light",
            Self::Scroll => "Scroll",
            Self::Wand => "Wand",
            Self::Staff => "Staff",
            Self::Weapon => "Weapon",
            Self::Fireweapon => "Fire Weapon",
            Self::Missile => "Missile",
            Self::Treasure => "Treasure",
            Self::Armor => "Armor",
            Self::Potion => "Potion",
            Self::Worn => "Worn",
            Self::Other => "Other",
            Self::Trash => "Trash",
            Self::Trap => "Trap",
            Self::Container => "Container",
            Self::Note => "Note",
            Self::Drinkcontainer => "Drink Container",
            Self::Key => "Key",
            Self::Food => "Food",
            Self::Money => "Money",
            Self::Pen => "Pen",
            Self::Boat => "Boat",
            Self::Fountain => "Fountain",
            Self::Portal => "Portal",
            Self::Rope => "Rope",
            Self::Spellbook => "Spellbook",
            Self::Wall => "Wall",
            Self::Touchstone => "Touchstone",
            Self::Board => "Board",
            Self::Instrument => "Instrument",
            Self::Vehicle => "Vehicle",
            Self::Corpse => "Corpse",
            Self::Kit => "Kit",
            Self::Wings => "Wings",
            Self::Perfume => "Perfume",
            Self::Disguise => "Disguise",
            Self::Poison => "Poison",
        }
    }
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = r#""AchievementCategory""#, rename_all = "SCREAMING_SNAKE_CASE")]
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
#[sqlx(type_name = r#""MobRole""#, rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MobRole {
    Trash,
    Normal,
    Elite,
    Miniboss,
    Boss,
    RaidBoss,
}

impl MobRole {
    /// Human-readable label for stat / mlist output. The
    /// `RaidBoss` variant collides on Debug output (no space) so we
    /// expand it; the rest stay one-word.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Trash => "Trash",
            Self::Normal => "Normal",
            Self::Elite => "Elite",
            Self::Miniboss => "Miniboss",
            Self::Boss => "Boss",
            Self::RaidBoss => "Raid Boss",
        }
    }
}

/// Special NPC service roles. A mob can carry multiple
/// professions (banker + receptionist, etc). Mirrors the schema
/// enum verbatim.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = r#""MobProfession""#, rename_all = "SCREAMING_SNAKE_CASE")]
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
#[sqlx(type_name = r#""Alignment""#, rename_all = "SCREAMING_SNAKE_CASE")]
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
#[sqlx(type_name = r#""ProtectedKind""#, rename_all = "SCREAMING_SNAKE_CASE")]
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
#[sqlx(type_name = r#""MobBehavior""#, rename_all = "SCREAMING_SNAKE_CASE")]
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

impl MobBehavior {
    /// Short human label for builder-facing output. Matches the
    /// schema spelling but spaces multi-word forms.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sentinel => "Sentinel",
            Self::StayZone => "Stay-Zone",
            Self::Scavenger => "Scavenger",
            Self::Track => "Track",
            Self::SlowTrack => "Slow-Track",
            Self::FastTrack => "Fast-Track",
            Self::Wimpy => "Wimpy",
            Self::Aware => "Aware",
            Self::Helper => "Helper",
            Self::Protector => "Protector",
            Self::Peacekeeper => "Peacekeeper",
            Self::NoBash => "No-Bash",
            Self::NoSummon => "No-Summon",
            Self::NoVicious => "No-Vicious",
            Self::Memory => "Memory",
            Self::Teacher => "Teacher",
            Self::Meditate => "Meditate",
            Self::NoScript => "No-Script",
            Self::NoClassAi => "No-ClassAI",
            Self::Peaceful => "Peaceful",
            Self::NoKill => "No-Kill",
        }
    }

    /// One-sentence description used by `mob-ai` to explain what
    /// each flag actually changes at runtime. Intentionally
    /// builder-facing wording — not for player-facing screens.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Sentinel => "Stays in its starting room — never wanders.",
            Self::StayZone => "Wanders, but never crosses zone boundaries.",
            Self::Scavenger => "Picks up loose items in the room.",
            Self::Track => "Will track a fleeing target through the zone.",
            Self::SlowTrack => "Tracks fleeing targets, but slowly.",
            Self::FastTrack => "Tracks fleeing targets relentlessly.",
            Self::Wimpy => "Flees when its HP gets low.",
            Self::Aware => "Notices sneaking / invisible characters.",
            Self::Helper => "Joins fights on the side of any other mob in the room.",
            Self::Protector => "Defends weaker mobs from being attacked.",
            Self::Peacekeeper => "Tries to break up player-vs-player fights.",
            Self::NoBash => "Can't be bashed (knocked over).",
            Self::NoSummon => "Can't be summoned out of its current room.",
            Self::NoVicious => "Won't deliver killing blows on fleeing targets.",
            Self::Memory => "Remembers attackers and re-aggroes on sight.",
            Self::Teacher => "Acts as a guildmaster / trainer for some abilities.",
            Self::Meditate => "Naturally regenerates faster when idle.",
            Self::NoScript => "Skipped by trigger dispatch (intentionally).",
            Self::NoClassAi => "Won't run the class-driven combat AI.",
            Self::Peaceful => "Refuses to engage in combat at all.",
            Self::NoKill => "Cannot be killed — damage is clamped above zero.",
        }
    }
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = r#""UserRole""#, rename_all = "SCREAMING_SNAKE_CASE")]
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
#[sqlx(type_name = r#""WearFlag""#, rename_all = "SCREAMING_SNAKE_CASE")]
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

impl WearFlag {
    /// Human-readable label for `identify` / `vwear` / proto readouts.
    /// Most are one-word and need no expansion; the multi-handed slots
    /// pick up an explicit "Two-handed" / "Main hand" form so they're
    /// not lower-cased and run-together.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Finger => "Finger",
            Self::Neck => "Neck",
            Self::Ear => "Ear",
            Self::Wrist => "Wrist",
            Self::Head => "Head",
            Self::Eyes => "Eyes",
            Self::Face => "Face",
            Self::Body => "Body",
            Self::About => "About body",
            Self::Arms => "Arms",
            Self::Hands => "Hands",
            Self::Waist => "Waist",
            Self::Belt => "Belt",
            Self::Legs => "Legs",
            Self::Feet => "Feet",
            Self::Tail => "Tail",
            Self::Mainhand => "Main hand",
            Self::Offhand => "Off hand",
            Self::Twohand => "Two-handed",
            Self::Badge => "Badge",
            Self::Hover => "Hover",
            Self::Disguise => "Disguise",
        }
    }
}

// `type_name` carries embedded quotes for the same reason as
// `Position`: sqlx 0.8 resolves the type OID at encode time via
// `SELECT $1::regtype::oid`, and the unquoted `PlayerFlag` is folded
// to lowercase by Postgres, failing with a runtime syntax error.
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
    /// Admin sanction — silenced on global channels (gossip /
    /// shout / quest channel). Set by `mute`, cleared by re-running
    /// `mute <name>`. Doesn't block `say` / `tell`.
    Muted,
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
            "MUTED" => Some(Self::Muted),
            _ => None,
        }
    }

    /// Flags that should only ever be toggled by Builder+ accounts.
    /// `cmd_toggle` consults this so the generic-toggle path can't
    /// be used to bypass the dedicated god-toggle commands.
    #[must_use]
    pub fn is_god_only(self) -> bool {
        matches!(self, Self::HolyLight | Self::ShowIds | Self::Muted)
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
            Self::Muted => "MUTED",
        }
    }
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = r#""Permission""#, rename_all = "SCREAMING_SNAKE_CASE")]
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

impl Permission {
    /// Human-readable label for `account` / `roles` output. The grant
    /// table stores `SCREAMING_SNAKE_CASE`; the Rust enum is CamelCase;
    /// the user-facing form mostly follows the enum but expands a few
    /// multi-word entries that read awkwardly otherwise.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Build => "Build",
            Self::Code => "Code",
            Self::Admin => "Admin",
            Self::God => "God",
            Self::Shutdown => "Shutdown",
            Self::Wizlock => "Wizlock",
            Self::Syslog => "Syslog",
            Self::Log => "Log",
            Self::Force => "Force",
            Self::Snoop => "Snoop",
            Self::Freeze => "Freeze",
            Self::Thaw => "Thaw",
            Self::Ban => "Ban",
            Self::Unban => "Unban",
            Self::Dc => "Disconnect",
            Self::Advance => "Advance",
            Self::Restore => "Restore",
            Self::Notitle => "No-title",
            Self::Squelch => "Squelch",
            Self::Teleport => "Teleport",
            Self::Transfer => "Transfer",
            Self::Summon => "Summon",
            Self::Invisible => "Invisible",
            Self::Nohassle => "No-hassle",
            Self::ZoneReset => "Zone Reset",
            Self::Wiznet => "Wiznet",
            Self::Olc => "OLC (legacy)",
        }
    }
}

/// Which kind of world entity owns an `EntityVariables` row. Mirrors the
/// schema's `EntityType` Prisma enum verbatim — `MOB` / `OBJECT` / `ROOM`.
/// The (entity_type, zone, id, key) tuple is the primary key on
/// `entity_variables`, so this enum is the discriminator that lets one
/// table back trigger-set vars for all three runtime entity shapes.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = r#""EntityType""#, rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EntityType {
    Mob,
    Object,
    Room,
}

impl EntityType {
    /// Human-readable single-word label. Used by admin readouts that
    /// dump an entity's variable bag and the unit-test diagnostics in
    /// `EntityVariableCache`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mob => "Mob",
            Self::Object => "Object",
            Self::Room => "Room",
        }
    }
}

/// Damage element categories — drives `ObjectResistance.element` and
/// (eventually) ability damage typing for the combat resist step.
/// Mirrors the schema's `ElementType` enum verbatim. SLASH/PIERCE/
/// CRUSH are physical sub-types; FIRE/COLD/SHOCK are elemental;
/// MENTAL/HOLY/UNHOLY/NECROTIC are mystic.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = r#""ElementType""#, rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ElementType {
    Physical,
    Slash,
    Pierce,
    Crush,
    Force,
    Sonic,
    Bleed,
    Fire,
    Cold,
    Water,
    Earth,
    Air,
    Shock,
    Acid,
    Poison,
    Radiant,
    Shadow,
    Holy,
    Unholy,
    Heal,
    Necrotic,
    Mental,
    Nature,
}

/// Boolean flags on an object proto — classic MUD `EXTRA_*` bits.
/// Stored as a Postgres `_ObjectFlag` array on `Objects.flags`. The
/// runtime reads them at spawn time and attaches them as the
/// `ObjectFlags` component so command handlers (look / examine /
/// give / drop) can gate on a flag without reaching back to the
/// proto store.
///
/// `no_pg_array` disables sqlx's auto-derived `PgHasArrayType` impl
/// so the manual impl below can supply the case-preserving quoted
/// array name (`_ObjectFlag`); the auto-derive lowercases.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = r#""ObjectFlag""#, rename_all = "SCREAMING_SNAKE_CASE", no_pg_array)]
pub enum ObjectFlag {
    Glow,
    Hum,
    Invisible,
    Magic,
    Permanent,
    Temporary,
    Decomposing,
    Float,
    Buoyant,
    Vehicle,
    Soulbound,
}

impl sqlx::postgres::PgHasArrayType for ObjectFlag {
    fn array_type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name(r#""_ObjectFlag""#)
    }
}

impl ObjectFlag {
    /// Short human label for `identify` / `examine` flag readouts.
    /// Variants stay one-word so the readout reads as a comma-list
    /// without awkward wrapping.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Glow => "Glow",
            Self::Hum => "Hum",
            Self::Invisible => "Invisible",
            Self::Magic => "Magic",
            Self::Permanent => "Permanent",
            Self::Temporary => "Temporary",
            Self::Decomposing => "Decomposing",
            Self::Float => "Float",
            Self::Buoyant => "Buoyant",
            Self::Vehicle => "Vehicle",
            Self::Soulbound => "Soulbound",
        }
    }
}

/// "Can't do that" restrictions on an object proto. Stored as a
/// Postgres `_ObjectRestriction` array on `Objects.restrictions`.
/// Each variant gates a specific command (`drop` / `get` / `sell`
/// / etc.); the gate fires before any state mutation so a NO_DROP
/// quest item can't accidentally land on the floor.
///
/// `no_pg_array` for the same case-preserved array-type reason as
/// `ObjectFlag` above.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(
    type_name = r#""ObjectRestriction""#,
    rename_all = "SCREAMING_SNAKE_CASE",
    no_pg_array
)]
pub enum ObjectRestriction {
    NoDrop,
    NoTake,
    NoSell,
    NoBurn,
    NoLocate,
    NoInvisible,
}

impl sqlx::postgres::PgHasArrayType for ObjectRestriction {
    fn array_type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name(r#""_ObjectRestriction""#)
    }
}

impl ObjectRestriction {
    /// Builder-facing label for `identify` / `examine` restriction
    /// readouts. Hyphenated form so "No-drop" reads as a single
    /// token in a comma-separated list.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NoDrop => "No-drop",
            Self::NoTake => "No-take",
            Self::NoSell => "No-sell",
            Self::NoBurn => "No-burn",
            Self::NoLocate => "No-locate",
            Self::NoInvisible => "No-invisible",
        }
    }
}

/// Body / form size class. Stored on `Mobs.size`; consumed by
/// bash-style takedowns (size disparity refusals), drag/throw checks,
/// mount eligibility, and the player-facing examine summary. Bands
/// match the schema enum verbatim; numeric `rank()` is the ordinal
/// used by gameplay gates that compare sizes ("bash refuses targets
/// more than one band larger than you").
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[sqlx(type_name = r#""Size""#, rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Size {
    Tiny,
    Small,
    Medium,
    Large,
    Huge,
    Giant,
    Gargantuan,
    Colossal,
    Titanic,
    Mountainous,
}

impl Size {
    /// Numeric ordinal — TINY=0 .. MOUNTAINOUS=9. Bash / drag /
    /// throw gates compare ranks; "more than one band bigger" =
    /// `target.rank() > attacker.rank() + 1`.
    #[must_use]
    pub const fn rank(self) -> i32 {
        match self {
            Self::Tiny => 0,
            Self::Small => 1,
            Self::Medium => 2,
            Self::Large => 3,
            Self::Huge => 4,
            Self::Giant => 5,
            Self::Gargantuan => 6,
            Self::Colossal => 7,
            Self::Titanic => 8,
            Self::Mountainous => 9,
        }
    }

    /// Human-readable label for `examine` / `stat mob`. All variants
    /// fit a single word and read cleanly inline.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::Huge => "huge",
            Self::Giant => "giant",
            Self::Gargantuan => "gargantuan",
            Self::Colossal => "colossal",
            Self::Titanic => "titanic",
            Self::Mountainous => "mountainous",
        }
    }
}

/// Life-force / vitality category. Mirrors the schema's `LifeForce`
/// enum and gates holy/unholy interactions: `detect_undead` reveals
/// UNDEAD-classed mobs, `turn_undead` / `holy_word` filter their
/// candidate lists by this tag, "celestial damage vs demonic" damage
/// bonus rules look it up. `LIFE` is the default for organic mobs
/// and incurs no special interactions.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = r#""LifeForce""#, rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LifeForce {
    Life,
    Undead,
    Magic,
    Celestial,
    Demonic,
    Elemental,
}

impl LifeForce {
    /// Human-readable label for `examine` / `stat mob`. Lower-cased
    /// because the examine sentence reads better as "Aura of unlife
    /// clings to it." rather than the raw enum spelling.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Life => "life",
            Self::Undead => "undead",
            Self::Magic => "magical",
            Self::Celestial => "celestial",
            Self::Demonic => "demonic",
            Self::Elemental => "elemental",
        }
    }
}

/// Natural attack flavor for a mob's unarmed swing — drives the
/// per-swing verb in combat narration ("The orc claws you." vs
/// "The wolf bites you."). Mirrors `Mobs.damage_type` in the
/// schema. Independent of `ElementType` (which tracks the resist
/// channel); `DamageType::FIRE` here is the mob's "shape" of
/// attack, not the resist axis.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = r#""DamageType""#, rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DamageType {
    Hit,
    Sting,
    Whip,
    Slash,
    Bite,
    Bludgeon,
    Crush,
    Pound,
    Claw,
    Maul,
    Thrash,
    Pierce,
    Blast,
    Punch,
    Stab,
    Fire,
    Cold,
    Acid,
    Shock,
    Poison,
    Align,
    Mental,
    Rot,
    Energy,
    Water,
}

impl DamageType {
    /// Third-person singular verb for the natural-attack swing line.
    /// Used by `attack_message` to render "The bear MAULS you."
    /// without each consumer hard-coding the inflection.
    #[must_use]
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Hit => "hits",
            Self::Sting => "stings",
            Self::Whip => "whips",
            Self::Slash => "slashes",
            Self::Bite => "bites",
            Self::Bludgeon => "bludgeons",
            Self::Crush => "crushes",
            Self::Pound => "pounds",
            Self::Claw => "claws",
            Self::Maul => "mauls",
            Self::Thrash => "thrashes",
            Self::Pierce => "pierces",
            Self::Blast => "blasts",
            Self::Punch => "punches",
            Self::Stab => "stabs",
            Self::Fire => "burns",
            Self::Cold => "freezes",
            Self::Acid => "scalds",
            Self::Shock => "shocks",
            Self::Poison => "poisons",
            Self::Align => "smites",
            Self::Mental => "psychically lashes",
            Self::Rot => "withers",
            Self::Energy => "blasts",
            Self::Water => "douses",
        }
    }

    /// Human-readable label for proto readouts (`mstat`). Title-case
    /// single word; `verb()` is for in-combat narration.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Hit => "Hit",
            Self::Sting => "Sting",
            Self::Whip => "Whip",
            Self::Slash => "Slash",
            Self::Bite => "Bite",
            Self::Bludgeon => "Bludgeon",
            Self::Crush => "Crush",
            Self::Pound => "Pound",
            Self::Claw => "Claw",
            Self::Maul => "Maul",
            Self::Thrash => "Thrash",
            Self::Pierce => "Pierce",
            Self::Blast => "Blast",
            Self::Punch => "Punch",
            Self::Stab => "Stab",
            Self::Fire => "Fire",
            Self::Cold => "Cold",
            Self::Acid => "Acid",
            Self::Shock => "Shock",
            Self::Poison => "Poison",
            Self::Align => "Align",
            Self::Mental => "Mental",
            Self::Rot => "Rot",
            Self::Energy => "Energy",
            Self::Water => "Water",
        }
    }
}

/// Identity flags on a mob — "what the mob IS" (orthogonal to
/// `MobBehavior` which tracks "how the mob ACTS"). Mirrors the
/// schema's `MobTrait` enum verbatim. Stored as a Postgres
/// `_MobTrait` array on `Mobs.traits`; per-instance copy lands on
/// the spawned entity as a `MobTraits` component.
///
/// `no_pg_array` for the same case-preserved array-type reason as
/// `ObjectFlag` above — the auto-derive lowercases the array name.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = r#""MobTrait""#, rename_all = "SCREAMING_SNAKE_CASE", no_pg_array)]
pub enum MobTrait {
    Illusion,
    Animated,
    PlayerPhantasm,
    Aquatic,
    Mount,
    Summoned,
    Pet,
}

impl sqlx::postgres::PgHasArrayType for MobTrait {
    fn array_type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name(r#""_MobTrait""#)
    }
}

impl MobTrait {
    /// Short human label for `mstat` / `mob-ai` readouts. Multi-
    /// word variants hyphenate so the comma-list stays scannable.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Illusion => "Illusion",
            Self::Animated => "Animated",
            Self::PlayerPhantasm => "Player-Phantasm",
            Self::Aquatic => "Aquatic",
            Self::Mount => "Mount",
            Self::Summoned => "Summoned",
            Self::Pet => "Pet",
        }
    }
}

/// Mob movement mode — orthogonal to `PostureKind` (which tracks
/// standing/sitting/etc). FLYING gates aerial combat / "soars
/// overhead" rendering; SWIMMING and UNDERWATER discriminate
/// surface vs submerged water; MOUNTED indicates the mob is
/// presently being ridden; ETHEREAL is the phased/incorporeal
/// state. Mirrors the schema's `MovementMode` enum verbatim.
#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = r#""MovementMode""#, rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MovementMode {
    Normal,
    Flying,
    Swimming,
    Underwater,
    Mounted,
    Ethereal,
}

impl MovementMode {
    /// Human-readable label for examine / proto readouts.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "ground",
            Self::Flying => "flying",
            Self::Swimming => "swimming",
            Self::Underwater => "submerged",
            Self::Mounted => "mounted",
            Self::Ethereal => "ethereal",
        }
    }
}

