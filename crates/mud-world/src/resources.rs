use std::collections::{HashMap, HashSet};

use bevy_ecs::prelude::*;

use mud_db::enums::EntityType;

/// Authored achievement catalog loaded at boot. Hooks reference
/// rows by their stable `code` string (e.g. `"first_kill"`,
/// `"zone_30_cleared"`); the catalog provides id/title/description
/// for the grant + display path. `by_id` lets the per-character
/// unlock list (loaded as ids) render as titles in `cmd_achievements`.
#[derive(Resource, Default, Debug)]
pub struct AchievementCatalog {
    pub by_code: HashMap<String, AchievementDef>,
    pub by_id: HashMap<i32, AchievementDef>,
}

#[derive(Debug, Clone)]
pub struct AchievementDef {
    pub id: i32,
    pub code: String,
    pub title: String,
    pub description: String,
    pub category: mud_db::enums::AchievementCategory,
    pub hidden: bool,
    pub sort_order: i32,
}

/// Index of currently-spawned per-house rooms. Lookup key is
/// `(house_id, local_index)`. Populated lazily by `cmd_home`
/// the first time a house is entered; cleared on no schedule
/// today (rooms persist across tick cycles, only despawn at
/// process exit). A future eviction tick can walk this and free
/// rooms whose owner has been offline for N minutes.
#[derive(Resource, Default, Debug)]
pub struct HousingIndex {
    pub by_key: HashMap<(i32, i32), bevy_ecs::prelude::Entity>,
}

/// Maps composite (zone, id) keys from the schema to live entities in the world.
/// Used by spawn/lookup code to translate DB references to runtime handles.
#[derive(Resource, Debug, Default)]
pub struct WorldKeyIndex {
    pub zones: HashMap<i32, Entity>,
    pub rooms: HashMap<(i32, i32), Entity>,
    /// Legacy `CircleMUD` vnum → composite (zone, id) lookup for rooms.
    /// Built at load time as `zone_id * 100 + room.id` → (zone, id).
    /// Used to decode `Portal.values.Destination` integers (which the
    /// import preserves in the legacy form). Collisions exist for
    /// zones with > 100 rooms (zone 30 goes up to id 499); the last
    /// loaded wins. `cmd_enter` falls back gracefully when a vnum
    /// resolves to nothing.
    pub legacy_vnums: HashMap<i32, (i32, i32)>,
}

/// Per-room environmental effects keyed by composite world key.
/// Loaded from the `RoomEnvironmentalEffect` junction table at
/// boot. The runtime applies each linked effect to a player on
/// arrival into the room — short duration so leaving lets the
/// effect decay naturally without bookkeeping at the move site.
#[derive(Resource, Debug, Default)]
pub struct RoomEnvironmentalEffects {
    pub by_room: HashMap<(i32, i32), Vec<i32>>,
}

/// One pre-parsed `GameConfig` value. Loader picks the variant
/// based on the row's `value_type` column; accessors match on
/// the variant rather than re-parsing the raw text on every
/// call. Combat-tick paths read the same row hundreds of times
/// per second — the parse-once shape eliminates that hot-path
/// allocation + parse overhead.
#[derive(Debug, Clone)]
pub enum ConfigValue {
    Int(i64),
    Bool(bool),
    Float(f64),
    Str(String),
    Json(String),
}

/// K/V tunables from the `GameConfig` table. Replaces the
/// per-callsite `pub(crate) const FOO_COST` constants — call
/// sites pass the legacy value as `default` to `get_*`, and a row
/// in the table overrides it. Rows that fail to parse against the
/// declared `value_type` are dropped at load time with a warning;
/// the call site sees the default at lookup, same as a missing row.
///
/// Storage shape: `(category, key) -> ConfigValue` (a tagged enum
/// of the parsed variants). Loader runs once at boot; accessors
/// just match on the variant. Admin reload would re-run the loader
/// pass and replace the resource — that's the follow-up.
#[derive(Resource, Debug, Default)]
pub struct RuntimeConfig {
    pub by_key: HashMap<(String, String), ConfigValue>,
}

impl RuntimeConfig {
    /// Read a `(category, key)` row as `i32`. Falls back to
    /// `default` when the row is missing or wasn't an INT-typed
    /// row. Used for skill stamina costs, alignment thresholds,
    /// and most other small integer tunables.
    #[must_use]
    pub fn get_i32(&self, category: &str, key: &str, default: i32) -> i32 {
        match self.by_key.get(&(category.to_string(), key.to_string())) {
            Some(ConfigValue::Int(v)) => i32::try_from(*v).unwrap_or(default),
            _ => default,
        }
    }

    /// Same as `get_i32` for `i64`. Used for housing prices and
    /// other cost values that exceed `i32` headroom.
    #[must_use]
    pub fn get_i64(&self, category: &str, key: &str, default: i64) -> i64 {
        match self.by_key.get(&(category.to_string(), key.to_string())) {
            Some(ConfigValue::Int(v)) => *v,
            _ => default,
        }
    }

    /// Boolean tunable. Returns the parsed `Bool` variant directly;
    /// `Int(0)` and `Int(non-zero)` also coerce to false/true so a
    /// row authored as INT still resolves cleanly.
    #[must_use]
    pub fn get_bool(&self, category: &str, key: &str, default: bool) -> bool {
        match self.by_key.get(&(category.to_string(), key.to_string())) {
            Some(ConfigValue::Bool(v)) => *v,
            Some(ConfigValue::Int(v)) => *v != 0,
            _ => default,
        }
    }

    /// `f64` tunable for weather-tick intervals, scaling factors,
    /// etc. Falls back to `default` on missing or wrong-type rows.
    /// Int-typed rows coerce via `as f64` — config values are small
    /// integers in practice, well within mantissa precision.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn get_f64(&self, category: &str, key: &str, default: f64) -> f64 {
        match self.by_key.get(&(category.to_string(), key.to_string())) {
            Some(ConfigValue::Float(v)) => *v,
            Some(ConfigValue::Int(v)) => *v as f64,
            _ => default,
        }
    }

    /// String tunable — returns the row's value verbatim. Used for
    /// things like "weather mode" labels. JSON-typed rows return
    /// their raw JSON text; callers parse on their own.
    #[must_use]
    pub fn get_string<'a>(&'a self, category: &str, key: &str, default: &'a str) -> &'a str {
        match self.by_key.get(&(category.to_string(), key.to_string())) {
            Some(ConfigValue::Str(v) | ConfigValue::Json(v)) => v.as_str(),
            _ => default,
        }
    }
}

/// Builder-authored static screens (MOTD, news, credits, policies,
/// imotd, …) loaded from the schema's `SystemText` table at boot.
/// Keyed by the row's stable `key` string. Call sites pass a
/// hardcoded fallback so an empty DB still renders a working
/// screen — once a builder edits the row in Muditor, it overrides
/// the fallback for everyone. The runtime never reads from disk
/// for this content; the only filesystem dependency is the DB
/// connection string at startup.
#[derive(Resource, Debug, Default)]
pub struct SystemTexts {
    pub by_key: HashMap<String, SystemTextEntry>,
}

#[derive(Debug, Clone)]
pub struct SystemTextEntry {
    /// Category enum label as text — `"LOGIN"` / `"SYSTEM"` /
    /// `"COMBAT"` / `"IMMORTAL"`. Today this is informational; if
    /// we want category-scoped reload or admin listing it's already
    /// here for the asking.
    pub category: String,
    pub title: Option<String>,
    pub content: String,
    /// Minimum viewer level to display. `0` = visible to everyone;
    /// higher gates staff-only screens (`imotd`).
    pub min_level: i32,
}

impl SystemTexts {
    /// Look up the content body for a key, gated by viewer level.
    /// Returns the row's `content` when the row exists and the
    /// viewer meets `min_level`, otherwise `None`. Pass
    /// [`i32::MAX`] to bypass the level gate (admin paths).
    #[must_use]
    pub fn content(&self, key: &str, viewer_level: i32) -> Option<&str> {
        self.by_key
            .get(key)
            .filter(|e| viewer_level >= e.min_level)
            .map(|e| e.content.as_str())
    }

    /// Same as [`Self::content`] but always falls back to the
    /// supplied default when the row is missing or the viewer is
    /// under-leveled. Convenience for `cmd_motd`-style call sites
    /// that always render *something*.
    #[must_use]
    pub fn content_or<'a>(&'a self, key: &str, viewer_level: i32, fallback: &'a str) -> &'a str {
        self.content(key, viewer_level).unwrap_or(fallback)
    }
}

/// Per-stage login flow text — banner, identifier prompt, password
/// prompts, error messages, character-creation steps. Loaded from
/// the schema's `LoginMessage` table at boot. Keyed by
/// `(stage, variant)` so we can A/B-test or theme variants without
/// schema changes; the lookup falls back to the `"default"` variant
/// when a non-default variant is requested but not present.
///
/// `stage` keys match the Prisma `LoginStage` enum labels verbatim
/// (`"WELCOME_BANNER"`, `"EMAIL_PROMPT"`, …) — call sites use the
/// same strings the schema defines. As with `SystemTexts`, every
/// call site carries a compile-time fallback so an empty DB still
/// boots a working login screen.
#[derive(Resource, Debug, Default)]
pub struct LoginMessages {
    pub by_key: HashMap<(String, String), String>,
}

impl LoginMessages {
    /// Look up the message for `(stage, variant)`. Falls back to
    /// the `"default"` variant for the same stage when the
    /// requested variant is missing — that's how we keep theme/AB
    /// callers safe without forcing every variant to be authored
    /// before launch.
    #[must_use]
    pub fn get(&self, stage: &str, variant: &str) -> Option<&str> {
        self.by_key
            .get(&(stage.to_string(), variant.to_string()))
            .or_else(|| {
                self.by_key
                    .get(&(stage.to_string(), "default".to_string()))
            })
            .map(String::as_str)
    }

    /// Convenience: lookup with a hardcoded fallback string. Used
    /// at call sites where rendering *something* is mandatory
    /// (the connect-time banner, prompts).
    #[must_use]
    pub fn get_or<'a>(&'a self, stage: &str, variant: &str, fallback: &'a str) -> &'a str {
        self.get(stage, variant).unwrap_or(fallback)
    }
}

/// Discord guild configuration loaded once at boot. The schema
/// pins this row to primary key 1, so the runtime treats it as a
/// singleton resource. Channel IDs are consumed by command handlers
/// that mirror in-game broadcasts to Discord (gossip channel),
/// admin events (login approvals + bans), and start/restart
/// announcements. `None` means the operator hasn't populated the
/// row — the runtime then treats the bot as disabled.
///
/// The bot itself runs out-of-process (Muditor-side); the Rust
/// runtime just publishes the channel IDs so a future outbound
/// message-queue wire-up can pick them up without DB hits per send.
#[derive(Resource, Debug, Default, Clone)]
pub struct DiscordConfigCatalog {
    pub enabled: bool,
    pub guild_id: Option<String>,
    pub gossip_channel_id: Option<String>,
    pub admin_channel_id: Option<String>,
    pub announcement_channel_id: Option<String>,
}

impl DiscordConfigCatalog {
    /// Convenience: is the bot wired up enough to send to the
    /// gossip channel? The guild row must exist, be enabled, AND
    /// the gossip channel must be set. Mirrors the same gate the
    /// admin / announcement channels apply.
    #[must_use]
    pub fn can_send_gossip(&self) -> bool {
        self.enabled
            && self.guild_id.is_some()
            && self.gossip_channel_id.is_some()
    }

    #[must_use]
    pub fn can_send_admin(&self) -> bool {
        self.enabled
            && self.guild_id.is_some()
            && self.admin_channel_id.is_some()
    }

    #[must_use]
    pub fn can_send_announcement(&self) -> bool {
        self.enabled
            && self.guild_id.is_some()
            && self.announcement_channel_id.is_some()
    }
}

/// Pending Discord-link verification codes. Populated by
/// `cmd_discord_link` when a player kicks off the verification
/// flow; consumed by the bot-side ingress (out-of-process today)
/// when it sees the matching `/verify <code>` arrive in the
/// configured gossip channel.
///
/// Storage shape: `user_id -> (discord_id, code, expires_at)`. One
/// pending request per user — re-running `discord link` overwrites
/// the entry rather than queuing.
///
/// In-memory only; clears on restart by design (a code that was
/// minted before a restart wouldn't be honored by the in-process
/// state machine anyway, even if it survived).
#[derive(Resource, Debug, Default)]
pub struct PendingDiscordLinks {
    pub by_user: HashMap<String, PendingDiscordLink>,
}

#[derive(Debug, Clone)]
pub struct PendingDiscordLink {
    pub discord_id: String,
    pub code: String,
    pub expires_at: std::time::Instant,
}

impl PendingDiscordLinks {
    /// Drop entries whose `expires_at` has passed. Called from a
    /// periodic tick so a stale entry doesn't sit in the map forever
    /// after the player abandoned the flow.
    pub fn expire_old(&mut self, now: std::time::Instant) -> usize {
        let before = self.by_user.len();
        self.by_user.retain(|_, e| e.expires_at > now);
        before - self.by_user.len()
    }
}

/// Catalog of builder-authored help articles loaded from the
/// `HelpEntry` table at boot. Each row carries one or more
/// `keywords` ("FIREBALL", "FIRE BALL") plus a `title`, body
/// `content`, and optional metadata (usage / sphere / duration /
/// category). The `help` command resolves a typed topic by
/// case-insensitive exact keyword match — substrings do *not*
/// match (a substring of `BALL` would otherwise hit `FIREBALL`,
/// `SNOWBALL`, etc.), but title-prefix matches surface when no
/// keyword hits exactly.
///
/// `min_level` gates visibility — wizard-tier articles stay
/// invisible until the viewer is at least that level. When more
/// than one entry shares a keyword (e.g. duplicate import or a
/// later builder addition with overlapping keyword), the caller
/// gets back an `AmbiguousMatches` list of titles instead of
/// picking one arbitrarily.
#[derive(Resource, Debug, Default)]
pub struct HelpCatalog {
    /// All entries, indexed by case-insensitive keyword. Multiple
    /// keywords on the same row each map back to the same entry id;
    /// duplicate keywords across rows produce a Vec<id> the lookup
    /// disambiguates by min_level filter then ambiguity check.
    pub by_keyword: HashMap<String, Vec<i32>>,
    /// All entries, indexed by id. Title-prefix fallback walks this
    /// map directly.
    pub entries: HashMap<i32, HelpEntry>,
}

#[derive(Debug, Clone)]
pub struct HelpEntry {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub min_level: i32,
    pub category: Option<String>,
    pub usage: Option<String>,
    pub duration: Option<String>,
    pub sphere: Option<String>,
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum HelpLookup {
    /// Exactly one matching entry the viewer is allowed to read.
    Found(HelpEntry),
    /// Multiple matching entries — the caller renders the titles so
    /// the player can `help <title>` to disambiguate.
    AmbiguousMatches(Vec<String>),
    /// No keyword or title-prefix match the viewer can see.
    NotFound,
}

impl HelpCatalog {
    /// Find an entry by exact keyword match (case-insensitive) or
    /// by case-insensitive title-prefix when no keyword hits. Entries
    /// the viewer can't see (their `min_level` exceeds `viewer_level`)
    /// are filtered before the ambiguity check, so a low-level player
    /// asking for `"slay"` skips over a staff-only `SLAY` entry instead
    /// of being told it exists.
    #[must_use]
    pub fn lookup(&self, keyword: &str, viewer_level: i32) -> HelpLookup {
        let needle = keyword.trim();
        if needle.is_empty() {
            return HelpLookup::NotFound;
        }
        let lower = needle.to_ascii_lowercase();

        // 1) Exact keyword hit.
        if let Some(ids) = self.by_keyword.get(&lower) {
            let visible: Vec<&HelpEntry> = ids
                .iter()
                .filter_map(|id| self.entries.get(id))
                .filter(|e| viewer_level >= e.min_level)
                .collect();
            match visible.len() {
                0 => {} // fall through to prefix match
                1 => return HelpLookup::Found(visible[0].clone()),
                _ => {
                    let mut titles: Vec<String> =
                        visible.iter().map(|e| e.title.clone()).collect();
                    titles.sort_unstable();
                    titles.dedup();
                    return if titles.len() == 1 {
                        // Same title under multiple ids — treat as a
                        // single match using the first entry.
                        HelpLookup::Found(visible[0].clone())
                    } else {
                        HelpLookup::AmbiguousMatches(titles)
                    };
                }
            }
        }

        // 2) Title-prefix fallback. A player who types `help fire`
        //    with no `FIRE` keyword still surfaces `FIREBALL`,
        //    `FIRESHIELD`, etc. — same shape as the SUGGESTIONS
        //    block in the command-help path.
        let mut matches: Vec<&HelpEntry> = self
            .entries
            .values()
            .filter(|e| viewer_level >= e.min_level)
            .filter(|e| e.title.to_ascii_lowercase().starts_with(&lower))
            .collect();
        matches.sort_by(|a, b| a.title.cmp(&b.title));
        match matches.len() {
            0 => HelpLookup::NotFound,
            1 => HelpLookup::Found(matches[0].clone()),
            _ => HelpLookup::AmbiguousMatches(
                matches.into_iter().map(|e| e.title.clone()).collect(),
            ),
        }
    }

    /// Distinct categories present in the visible catalog, sorted
    /// alphabetically. Used by `help` with no args to render a
    /// "type help <topic>" index gated by viewer level.
    #[must_use]
    pub fn visible_categories(&self, viewer_level: i32) -> Vec<String> {
        let mut cats: Vec<String> = self
            .entries
            .values()
            .filter(|e| viewer_level >= e.min_level)
            .filter_map(|e| e.category.clone())
            .collect();
        cats.sort_unstable();
        cats.dedup();
        cats
    }

    /// Entry count visible to the viewer. Used for the empty-arg
    /// help screen ("123 articles available").
    #[must_use]
    pub fn visible_count(&self, viewer_level: i32) -> usize {
        self.entries
            .values()
            .filter(|e| viewer_level >= e.min_level)
            .count()
    }
}

/// Per-race defaults loaded from the schema's `Race` table at boot.
/// Today only `default_size` is wired (used by the score sheet); the
/// fuller surface (`focusBonus` / lifeforce / weight-height ranges)
/// lands here as features need it. Race name keys are the raw enum
/// labels (`HUMAN` / `ELF` / `HUMANOID` / ...) — same shape as
/// `Profile.race`.
#[derive(Resource, Debug, Default)]
pub struct RaceDefaults {
    /// `(race_label) -> Size` enum text (`MEDIUM` / `LARGE` / ...).
    /// Empty when the `Race` table has no rows yet (fresh DB) or
    /// hasn't been loaded.
    pub size_by_race: HashMap<String, String>,
    /// `(race_label) -> (zone_id, room_id)` from `Races.start_room_*`.
    /// Used as the spawn fallback when a character has no persisted
    /// `current_room` and no recall set yet — e.g. a fresh character
    /// or one whose persisted room no longer loads. Races with NULL
    /// `start_room` columns are absent from the map; the caller falls
    /// through to the Void fallback.
    pub start_room_by_race: HashMap<String, (i32, i32)>,
}

/// Full per-race catalog loaded from `Races` at boot. The narrow
/// `RaceDefaults` map is kept alongside for callers that only need
/// the size / start-room lookups; this catalog carries the rest of
/// the row so character-creation, score, combat, and rendering can
/// all read a single authoritative source. Keys are the raw `Race`
/// enum text (`HUMAN` / `ELF` / ...) — same shape as
/// `Profile.race`. Empty when the `Race` table has no rows yet.
#[derive(Resource, Debug, Default)]
pub struct RaceCatalog {
    pub by_race: HashMap<String, RaceDef>,
}

impl RaceCatalog {
    #[must_use]
    pub fn get(&self, race: &str) -> Option<&RaceDef> {
        self.by_race.get(race)
    }

    /// Inclusive height range for the given race + gender. Falls
    /// back to the male range when `gender` doesn't match the
    /// schema's `male`/`female` axis (the schema only authors two
    /// height bands; `neutral` / other strings land on the male
    /// band, which is what the legacy code did). Returns `None`
    /// when the race isn't in the catalog OR the relevant low/high
    /// columns are both zero (unauthored), so the caller can fall
    /// through to a fixed default instead of generating a `0`.
    #[must_use]
    pub fn height_range(&self, race: &str, gender: &str) -> Option<(i32, i32)> {
        let def = self.by_race.get(race)?;
        let (low, high) = if gender.eq_ignore_ascii_case("female") {
            (def.female_height_low, def.female_height_high)
        } else {
            (def.male_height_low, def.male_height_high)
        };
        if low == 0 && high == 0 {
            return None;
        }
        Some((low, high.max(low)))
    }

    /// Inclusive weight range for the given race + gender. Same
    /// gender-fallback semantics as `height_range`.
    #[must_use]
    pub fn weight_range(&self, race: &str, gender: &str) -> Option<(i32, i32)> {
        let def = self.by_race.get(race)?;
        let (low, high) = if gender.eq_ignore_ascii_case("female") {
            (def.female_weight_low, def.female_weight_high)
        } else {
            (def.male_weight_low, def.male_weight_high)
        };
        if low == 0 && high == 0 {
            return None;
        }
        Some((low, high.max(low)))
    }

    /// Roll a fresh height for a character of the given race +
    /// gender. Inclusive range. `None` when the catalog doesn't
    /// carry an authored band; caller falls through to a fixed
    /// default rather than persisting a meaningless 0.
    #[must_use]
    pub fn random_height(&self, race: &str, gender: &str) -> Option<i32> {
        let (low, high) = self.height_range(race, gender)?;
        if high <= low {
            return Some(low);
        }
        Some(rand::random_range(low..=high))
    }

    /// Roll a fresh weight for a character of the given race +
    /// gender. Same shape as `random_height`.
    #[must_use]
    pub fn random_weight(&self, race: &str, gender: &str) -> Option<i32> {
        let (low, high) = self.weight_range(race, gender)?;
        if high <= low {
            return Some(low);
        }
        Some(rand::random_range(low..=high))
    }

    /// Stat-cap clamp helper. Returns the catalog's per-race max
    /// for the named stat (`"strength"` / `"str"` / `"dex"` / ...),
    /// or the supplied `fallback` when the race isn't authored.
    /// Match is case-insensitive on the leading letters so command
    /// handlers can pass through user input without normalizing.
    #[must_use]
    pub fn stat_cap(&self, race: &str, stat: &str, fallback: i32) -> i32 {
        let Some(def) = self.by_race.get(race) else {
            return fallback;
        };
        match stat.to_ascii_lowercase().as_str() {
            "str" | "strength" => def.stat_caps.strength,
            "dex" | "dexterity" => def.stat_caps.dexterity,
            "con" | "constitution" => def.stat_caps.constitution,
            "int" | "intelligence" => def.stat_caps.intelligence,
            "wis" | "wisdom" => def.stat_caps.wisdom,
            "cha" | "charisma" => def.stat_caps.charisma,
            _ => fallback,
        }
    }
}

/// Parse a `{"FIRE": 25, "COLD": -10}`-style JSON object into the
/// typed `ElementType` map used by the runtime. Keys are matched
/// case-insensitively against the SCREAMING_SNAKE schema labels;
/// unknown keys and non-number values are dropped silently. Shared
/// between race + class catalog hydration so the parsing rules
/// stay in one place.
#[must_use]
pub fn parse_resistance_json(
    raw: &serde_json::Value,
) -> HashMap<mud_db::enums::ElementType, i32> {
    use mud_db::enums::ElementType;
    let mut out: HashMap<ElementType, i32> = HashMap::new();
    let Some(obj) = raw.as_object() else {
        return out;
    };
    for (k, v) in obj {
        let element = match k.to_ascii_uppercase().as_str() {
            "PHYSICAL" => ElementType::Physical,
            "SLASH" => ElementType::Slash,
            "PIERCE" => ElementType::Pierce,
            "CRUSH" => ElementType::Crush,
            "FORCE" => ElementType::Force,
            "SONIC" => ElementType::Sonic,
            "BLEED" => ElementType::Bleed,
            "FIRE" => ElementType::Fire,
            "COLD" => ElementType::Cold,
            "WATER" => ElementType::Water,
            "EARTH" => ElementType::Earth,
            "AIR" => ElementType::Air,
            "SHOCK" => ElementType::Shock,
            "ACID" => ElementType::Acid,
            "POISON" => ElementType::Poison,
            "RADIANT" => ElementType::Radiant,
            "SHADOW" => ElementType::Shadow,
            "HOLY" => ElementType::Holy,
            "UNHOLY" => ElementType::Unholy,
            "HEAL" => ElementType::Heal,
            "NECROTIC" => ElementType::Necrotic,
            "MENTAL" => ElementType::Mental,
            "NATURE" => ElementType::Nature,
            _ => continue,
        };
        if let Some(n) = v.as_i64() {
            let clamped = i32::try_from(n.clamp(i64::from(i32::MIN), i64::from(i32::MAX)))
                .unwrap_or(0);
            out.insert(element, clamped);
        } else if let Some(f) = v.as_f64() {
            #[allow(clippy::cast_possible_truncation)]
            let n = f.round() as i32;
            out.insert(element, n);
        }
    }
    out
}

/// Stat-cap bundle. Mirrors the per-attribute layout of the
/// schema's `Races.max_*` columns. The runtime keeps a separate
/// struct (rather than reusing `components::CoreStats`) so
/// `mud-world` doesn't have to import that component into the
/// resource layer.
#[derive(Debug, Clone, Copy)]
pub struct RaceStatCaps {
    pub strength: i32,
    pub dexterity: i32,
    pub constitution: i32,
    pub intelligence: i32,
    pub wisdom: i32,
    pub charisma: i32,
}

impl Default for RaceStatCaps {
    fn default() -> Self {
        // Mirrors the schema defaults (76 for every column).
        Self {
            strength: 76,
            dexterity: 76,
            constitution: 76,
            intelligence: 76,
            wisdom: 76,
            charisma: 76,
        }
    }
}

/// One row of `RaceCatalog`. Holds the full schema column set
/// (plus a pre-computed `stat_caps` bundle) so command and combat
/// handlers don't have to re-aggregate per call. Resistance JSON
/// is distilled at hydration into a typed `ElementType` map; the
/// raw JSON is kept alongside in case authoring metadata needs to
/// round-trip without lossiness.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct RaceDef {
    /// Raw `Race` enum text — same shape as `Profile.race`.
    pub race: String,
    /// Display name (XML-Lite color tags).
    pub name: String,
    pub plain_name: String,
    /// Space-separated keyword list (lookup convenience — split on
    /// whitespace at the call site).
    pub keywords: String,
    pub playable: bool,
    pub humanoid: bool,
    pub magical: bool,
    /// `RaceAlign` enum text (`GOOD` / `NEUTRAL` / `EVIL`).
    pub race_align: String,
    pub default_alignment: i32,
    /// `Size` enum text — duplicated into `RaceDefaults.size_by_race`
    /// for the narrow lookup, also carried here so the catalog is
    /// self-contained.
    pub default_size: String,
    pub focus_bonus: i32,
    /// `LifeForce` enum text.
    pub default_lifeforce: String,
    pub male_weight_low: i32,
    pub male_weight_high: i32,
    pub male_height_low: i32,
    pub male_height_high: i32,
    pub female_weight_low: i32,
    pub female_weight_high: i32,
    pub female_height_low: i32,
    pub female_height_high: i32,
    /// Pre-assembled stat-cap bundle so the cap-check at training
    /// / rolling time is one field access. Built from the six
    /// `max_*` columns at hydration time.
    pub stat_caps: RaceStatCaps,
    /// XP gain multiplier in percent. `100` = unchanged; `120` →
    /// each kill awards 1.2× XP. Schema default `100`.
    pub exp_factor: i32,
    /// HP gain multiplier in percent. Applied on top of
    /// `LevelDefinition.hp_gain` at level-up.
    pub hp_factor: i32,
    /// Damage scalar in percent. Each swing's damage gets scaled
    /// by `damage * hit_damage_factor / 100`.
    pub hit_damage_factor: i32,
    /// Per-die damage scalar. Applied to natural-damage dice rolls
    /// (claws / teeth) before the per-swing `hit_damage_factor`.
    pub damage_dice_factor: i32,
    /// Coin-drop scalar in percent. Drops awarded to a player of
    /// this race get scaled by `coin * copper_factor / 100`.
    pub copper_factor: i32,
    /// Movement-broadcast override for arrivals. `Some("swoops
    /// down")` means the room sees "The pegasus swoops down from
    /// the south."; `None` falls back to the default "arrives
    /// from <dir>".
    pub enter_verb: Option<String>,
    /// Movement-broadcast override for departures. Paired with
    /// `enter_verb` semantically.
    pub leave_verb: Option<String>,
    pub start_room_zone_id: Option<i32>,
    pub start_room_id: Option<i32>,
    /// Per-element resistance map. Parsed once at hydration from
    /// the schema's `Races.resistances` JSON — keys outside the
    /// runtime's `ElementType` set are dropped, keys are matched
    /// case-insensitively against the SCREAMING_SNAKE schema label.
    /// Empty when the row has `{}` or NULL.
    pub resistances: HashMap<mud_db::enums::ElementType, i32>,
    /// Original JSON blob, kept for round-trip / authoring debug.
    pub resistances_raw: serde_json::Value,
}

/// Catalog of effect *types* loaded from the Effect table at startup.
/// Active applications live as ECS entities (`EffectInstance` + `AppliedTo`);
/// the catalog supplies metadata that doesn't change per-application.
#[derive(Resource, Debug, Default)]
pub struct EffectCatalog {
    pub by_id: HashMap<i32, EffectDef>,
}

impl EffectCatalog {
    #[must_use] 
    pub fn find_by_name(&self, name: &str) -> Option<&EffectDef> {
        self.by_id
            .values()
            .find(|e| e.name.eq_ignore_ascii_case(name))
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct EffectDef {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    pub effect_type: String,
    pub tags: Vec<String>,
    pub presence_override: Option<String>,
    /// JSONB blob of default parameters from `Effect.default_params`.
    /// Used as the secondary fallback for duration/amount/etc. when an
    /// `AbilityEffect.override_params` row didn't supply them.
    pub default_params: serde_json::Value,
    /// Mirror of the schema's prevent-flags. The dispatcher reads
    /// these at command time to refuse silenced/held/anti-magic
    /// actions. None of the seeded fierydev rows use them today; the
    /// runtime is pre-wired so when content lands the gate works.
    pub prevents_speaking: bool,
    pub prevents_casting: bool,
    pub prevents_movement: bool,
    /// Lua source executed when an instance is applied. `self`
    /// binds to the target entity. None / blank means "no hook".
    pub on_apply: Option<String>,
    /// Lua source executed once per second while the effect is
    /// active. `self` binds to the target.
    pub on_tick: Option<String>,
    /// Lua source executed when the effect is removed (expiry,
    /// dispel, cleanse, target despawn). `self` binds to the target.
    pub on_remove: Option<String>,
}

/// Catalog of object prototypes loaded from the Objects table at startup.
/// Spawning a real instance copies the relevant fields onto a new entity.
#[derive(Resource, Debug, Default)]
pub struct ObjectPrototypes {
    pub by_key: HashMap<(i32, i32), ObjectProto>,
}

/// One ability binding on an object proto. `charges = None` means
/// unlimited; otherwise it's a finite-use charge count (wands).
#[derive(Debug, Clone, Copy)]
pub struct ObjectAbilityBinding {
    pub ability_id: i32,
    pub level: i32,
    pub charges: Option<i32>,
}

/// Catalog of `ObjectAbilities` rows: per-proto list of bound
/// abilities. Read by `recite` / `wave` / `tap` and (today) by
/// `stat` for diagnostics.
#[derive(Resource, Debug, Default)]
pub struct ObjectAbilityCatalog {
    pub by_key: HashMap<(i32, i32), Vec<ObjectAbilityBinding>>,
}

/// One row from `ConsumableEffects`. The schema binds either a
/// specific Object proto (zone+id) OR a Liquid (id) — not both — so
/// this catalog stores both maps and the consume handler queries
/// the appropriate one.
#[derive(Debug, Clone, Copy)]
pub struct ConsumableEffectBinding {
    pub effect_id: i32,
    pub chance: f64,
    pub level: i32,
    pub duration_secs: Option<i32>,
}

/// Lightweight catalog of `Liquids` rows — name → id mapping used
/// by the drink path to look up `ConsumableEffects` per-liquid
/// bindings. Names are normalized to lowercase at insert.
/// `drunk_effect` is the per-unit alcohol contribution (0 for
/// non-alcoholic drinks).
///
/// Kept as a thin sibling of `LiquidCatalog`: callers that only
/// need the id (effect dispatch, drunk delta) avoid pulling the
/// full `LiquidDef`.
#[derive(Resource, Debug, Default)]
pub struct LiquidIndex {
    pub by_name: HashMap<String, i32>,
    pub drunk_effect: HashMap<String, i32>,
}

/// Rich catalog of `Liquids` rows hydrated at boot. Indexed by
/// alias (the single-token keyword carried on
/// `LiquidContainer.liquid`) and by id. Drink commands look up the
/// def to fetch color, hunger/thirst/drunk deltas, and flavor
/// description; `pour`/`fill` use it to canonicalize the alias
/// stored on a refilled container.
///
/// Pair with `LiquidIndex` — both populate from the same DB pass.
/// `LiquidCatalog` carries the full per-row payload; `LiquidIndex`
/// stays as the leaner shape consumed by hot paths.
#[derive(Resource, Default, Debug, Clone)]
pub struct LiquidCatalog {
    by_alias: HashMap<String, LiquidDef>,
    by_id: HashMap<i32, LiquidDef>,
}

#[derive(Debug, Clone)]
pub struct LiquidDef {
    pub id: i32,
    pub name: String,
    pub alias: String,
    pub color_desc: String,
    pub drunk_effect: i32,
    pub hunger_effect: i32,
    pub thirst_effect: i32,
    pub description: Option<String>,
}

impl LiquidCatalog {
    /// Insert a row. `alias` is matched case-insensitively, so we
    /// normalize at insert. A duplicate alias (shouldn't happen —
    /// the DB has a UNIQUE on `alias`) overwrites the prior entry.
    pub fn insert(&mut self, def: LiquidDef) {
        self.by_id.insert(def.id, def.clone());
        self.by_alias.insert(def.alias.to_ascii_lowercase(), def);
    }

    /// Look up by alias / keyword (case-insensitive). Aliases match
    /// what `LiquidContainer.liquid` carries on a spawned item.
    #[must_use]
    pub fn lookup_alias(&self, alias: &str) -> Option<&LiquidDef> {
        self.by_alias.get(&alias.to_ascii_lowercase())
    }

    /// Look up by schema id (`Liquids.id`).
    #[must_use]
    pub fn lookup_id(&self, id: i32) -> Option<&LiquidDef> {
        self.by_id.get(&id)
    }

    /// Number of rows loaded — used by the loader's boot summary
    /// log line.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// "Water-like" fallback used when a `LiquidContainer` carries
    /// an alias the catalog doesn't know (legacy import drift,
    /// hand-edited DB rows). Picks the real `water` entry when it
    /// exists; otherwise synthesizes a minimal default so the
    /// drink path stays usable.
    #[must_use]
    pub fn fallback(&self) -> LiquidDef {
        self.lookup_alias("water").cloned().unwrap_or(LiquidDef {
            id: 0,
            name: "water".to_string(),
            alias: "water".to_string(),
            color_desc: "clear".to_string(),
            drunk_effect: 0,
            hunger_effect: 0,
            thirst_effect: 10,
            description: None,
        })
    }
}

/// Catalog of `ConsumableEffects` rows: which effects fire on
/// eat/drink/quaff for a given object proto or liquid. Read by
/// `consume_item` and the drink handlers right before the
/// despawn / liquid-decrement.
#[derive(Resource, Debug, Default)]
pub struct ConsumableEffectCatalog {
    pub by_object: HashMap<(i32, i32), Vec<ConsumableEffectBinding>>,
    pub by_liquid: HashMap<i32, Vec<ConsumableEffectBinding>>,
}

#[derive(Debug, Clone)]
pub struct ObjectProto {
    pub zone_id: i32,
    pub id: i32,
    pub r#type: mud_db::enums::ObjectType,
    pub name: String,
    pub keywords: Vec<String>,
    /// Short line shown in a room's "On the ground:" listing.
    pub room_description: String,
    /// Long description shown by `examine`. None means "fall back to name".
    pub examine_description: Option<String>,
    pub weight: f64,
    pub level: i32,
    /// Wear-slot flags from the schema; spawned items derive a single
    /// primary `WearableIn` from the first relevant flag (see
    /// `wear_flags_to_slot`).
    pub wear_flags: Vec<mud_db::enums::WearFlag>,
    /// Weapon dice expression `NdM+B`. Read directly from typed
    /// `Objects.weapon_dice_*` columns at load time — no JSONB
    /// extraction. Zero for non-weapons. `avg_damage()` uses these
    /// to resolve the formula evaluator's `weapon_damage` symbol
    /// when this proto is the caster's wielded item.
    pub weapon_dice_num: i32,
    pub weapon_dice_size: i32,
    pub weapon_dice_bonus: i32,
    /// Weapon damage type label (`Slash` / `Pierce` / `Crush` / ...)
    /// from `Objects.weapon_damage_type` typed column. `None` for
    /// non-weapons or weapons without an authored type.
    pub weapon_damage_type: Option<String>,
    /// Armor mitigation percent (already pre-scaled at fierylib
    /// import time via legacy `Objects.values.AC` × 2). Read
    /// directly from the typed `Objects.armor_pct` column.
    /// `apply_object_to_wearer` folds this into the wearer's
    /// `CombatStats.armor_pct` via `apply_modify_delta(wearer,
    /// "armor_pct", armor_pct)`. Zero for non-armor protos.
    pub armor_pct: i32,
    /// Base value in copper (the schema's `Objects.cost`). Shops will
    /// pay some fraction of this on sell; appraisal commands surface
    /// the raw number split into denominations.
    pub cost: i32,
    /// `Portal`-typed objects only: the legacy `CircleMUD` vnum of the
    /// destination room (`Objects.values.Destination`). `None` for
    /// non-portal protos and for portals with no/zero destination.
    /// Decode via `WorldKeyIndex.legacy_vnums`.
    pub portal_destination_vnum: Option<i32>,
    /// `Board`-typed objects only: the `Board.id` they reference,
    /// pulled from `Objects.values.Pages` (legacy convention — that
    /// field doubles as the board id). `None` for non-boards or
    /// when the value is missing/zero.
    pub board_id: Option<i32>,
    /// `DrinkContainer`-typed objects only: initial liquid state at
    /// spawn time. Each spawned instance gets a fresh
    /// `LiquidContainer` component built from these values; mutation
    /// happens per-instance (drink/sip/pour/fill) without touching
    /// the proto.
    pub liquid: Option<LiquidProto>,
    /// Per-spawn fuel state for Light-type items. Schema's
    /// `Objects.values` has `Capacity` / `Remaining` in game-hours;
    /// `Remaining < 0` means "infinite" (eternal-flame items).
    pub light_fuel: Option<LightFuelProto>,
    /// Alignments that CAN'T equip this item. Empty for
    /// unrestricted gear; checked at wear time against the
    /// player's three-bucket alignment.
    pub restricted_alignments: Vec<mud_db::enums::Alignment>,
    /// Class ids that CAN'T equip this item. Empty for
    /// unrestricted gear.
    pub restricted_class_ids: Vec<i32>,
    /// Races that CAN'T equip this item, stored as the raw
    /// enum-label string (HUMAN / ELF / ...) for direct
    /// comparison with `Profile.race`.
    pub restricted_races: Vec<String>,
    /// `ObjectExtraDescriptions` for this proto: keyword-addressable
    /// flavor text addressable from `examine <keyword>` against the
    /// item ("look pommel" on an ornate sword shows the jeweled
    /// pommel detail). Empty for plain items.
    pub extras: Vec<(Vec<String>, String)>,
    /// `ObjectResistance` rows for this proto: per-element resistance
    /// percentages applied to the wearer while equipped. Empty for
    /// items with no resistance grants.
    pub resistances: Vec<(mud_db::enums::ElementType, i32, bool)>,
    /// `ObjectEffects` rows for this proto: spell-like effects spawned
    /// onto the wearer while equipped. Each entry pairs an `Effect.id`
    /// with a strength and an optional slot restriction (only fires
    /// when worn in `wear_location`). Empty for items with no granted
    /// effects.
    pub granted_effects: Vec<ObjectGrantedEffect>,
    /// Boolean attribute flags from `Objects.flags`: GLOW / HUM /
    /// INVISIBLE / MAGIC / PERMANENT / TEMPORARY / DECOMPOSING /
    /// FLOAT / BUOYANT / VEHICLE / SOULBOUND. Stamped on the spawned
    /// entity as an `ObjectFlags` component when non-empty;
    /// consumers (look / examine / drop / give) gate on it through
    /// the component rather than reaching back to the proto.
    pub flags: Vec<mud_db::enums::ObjectFlag>,
    /// "Can't do that" restriction flags from `Objects.restrictions`:
    /// NO_DROP / NO_TAKE / NO_SELL / NO_BURN / NO_LOCATE /
    /// NO_INVISIBLE. Per-command gates consult these before
    /// mutating world state so a quest item never lands on the
    /// floor by accident.
    pub restrictions: Vec<mud_db::enums::ObjectRestriction>,
    /// Lifetime ticker (B1). Positive = item decays after this many
    /// game-hours; spawn-time wiring converts to seconds (×75) and
    /// attaches an `ItemTimer` component. PERMANENT flag bypasses
    /// the wire so eternal-flame fixtures don't pop.
    pub timer_hours: i32,
    /// Post-timer decompose window. Non-zero = the item gets the
    /// DECOMPOSING semantic after `timer_hours` expires (a second
    /// countdown before destruction). Unused today — the runtime
    /// just destroys when timer hits zero. Kept on the proto so a
    /// later two-phase decay can read it without a schema bump.
    pub decompose_timer: i32,
    /// Inclusive race allow-list (B6). Empty = anyone, non-empty =
    /// only listed races may wear. Independent of
    /// `restricted_races` (deny-list) — both can be set.
    pub allowed_races: Vec<String>,
    /// Minimum body size to wear (B6). `None` = no floor.
    pub min_size: Option<String>,
    /// Maximum body size to wear (B6). `None` = no ceiling.
    pub max_size: Option<String>,
}

/// One `ObjectEffects` row, denormalized into the proto.
#[derive(Debug, Clone)]
pub struct ObjectGrantedEffect {
    pub effect_id: i32,
    pub strength: i32,
    pub modifier_data: serde_json::Value,
    /// When `Some(slot)`, the effect only fires while the item is
    /// equipped in that legacy wear-flag slot. `None` = any slot.
    pub wear_location: Option<mud_db::enums::WearFlag>,
}

/// Static initial-spawn data for a `DrinkContainer` proto. Mirrors
/// the schema's `Objects.values` shape for these items.
#[derive(Debug, Clone)]
pub struct LiquidProto {
    pub liquid: String,
    pub capacity: i32,
    pub remaining: i32,
    pub poisoned: bool,
}

/// Static initial-spawn data for a Light-type item's fuel.
/// `remaining < 0` (and `capacity < 0`) means infinite — eternal
/// flames, magical glow-globes, and the like.
#[derive(Debug, Clone, Copy)]
pub struct LightFuelProto {
    pub capacity: i32,
    pub remaining: i32,
}

impl ObjectProto {
    /// Average damage roll: `N * (M + 1) / 2 + B`. Returns 0 for
    /// non-weapons (zero dice) so callers can use it directly.
    #[must_use]
    pub fn avg_damage(&self) -> i32 {
        let n = self.weapon_dice_num;
        let m = self.weapon_dice_size;
        let b = self.weapon_dice_bonus;
        if n <= 0 || m <= 0 {
            return b.max(0);
        }
        n * (m + 1) / 2 + b
    }
}

/// Catalog of mob prototypes loaded from the Mobs table at startup. The
/// `summon` admin command and (eventually) the `MobReset` spawner read this
/// to materialize fresh mob entities.
#[derive(Resource, Debug, Default)]
pub struct MobPrototypes {
    pub by_key: HashMap<(i32, i32), MobProto>,
}

/// Per-shop offering: an item the keeper sells, with stock and the
/// override price (`0` = use the object's base cost).
#[derive(Debug, Clone, Copy)]
pub struct ShopOffering {
    pub object_zone_id: i32,
    pub object_id: i32,
    /// `-1` = unlimited stock.
    pub amount: i32,
    /// Override price in copper; `0` falls back to the proto's base
    /// cost multiplied by the shop's `buy_profit`.
    pub price: i32,
}

/// Per-shop sell whitelist: an `ObjectType` plus an optional keyword
/// filter. Empty `keywords` means "accept any item of this type";
/// non-empty means at least one of these keywords must match the
/// item's `Keywords`.
#[derive(Debug, Clone)]
pub struct ShopAcceptRule {
    pub object_type: String,
    pub keywords: Vec<String>,
}

/// Pet-shop offering: a mob the keeper sells. Mirrors `ShopMobs`.
/// Pet shops list mobs the player can `hire`, becoming following
/// pets. `price = 0` means "use mob.level * 100" by schema convention.
#[derive(Debug, Clone, Copy)]
pub struct ShopPetOffering {
    pub mob_zone_id: i32,
    pub mob_id: i32,
    /// `-1` = unlimited stock.
    pub amount: i32,
    pub price: i32,
}

/// One entry in `ShopCatalog`. Keyed by the shop's `(zone_id, id)`;
/// resolved from a keeper mob via `keeper_index`.
#[derive(Debug, Clone)]
pub struct ShopDef {
    pub zone_id: i32,
    pub id: i32,
    pub keeper_zone_id: i32,
    pub keeper_id: i32,
    pub buy_profit: f64,
    pub sell_profit: f64,
    pub items: Vec<ShopOffering>,
    /// Sell-side filter rows. Empty Vec = accept anything (no filter
    /// row in `ShopAccepts` for this shop). Non-empty = the item must
    /// match at least one rule for `sell` to succeed.
    pub accepts: Vec<ShopAcceptRule>,
    /// Pet shop offerings (see `ShopPetOffering`). Empty for non-pet
    /// shops; populated only for shops with `ShopMobs` rows.
    pub pets: Vec<ShopPetOffering>,
}

/// One row from the `Board` table cached at load time. Used by the
/// in-room board-object renderer: when a player examines a
/// BOARD-typed item, its `BoardLink(id)` looks up here for the
/// alias/title to print in the hint.
#[derive(Debug, Clone)]
pub struct BoardSummary {
    pub id: i32,
    pub alias: String,
    pub title: String,
    pub locked: bool,
}

/// Catalog of every message board, keyed by `Board.id`. Snapshot
/// taken at startup; message counts and edit state aren't cached
/// here because they change at runtime — the actual `board <alias>`
/// command queries live data.
#[derive(Resource, Debug, Default)]
pub struct BoardCatalog {
    pub by_id: HashMap<i32, BoardSummary>,
}

/// Lua trigger body + metadata cached by `(zone_id, id)`. Loaded
/// once at startup. The runtime never compiles these eagerly —
/// the dispatcher reads `commands` lazily per-fire when the entity
/// the trigger is attached to needs to run it.
#[derive(Debug, Clone)]
pub struct TriggerDef {
    pub zone_id: i32,
    pub id: i32,
    pub name: String,
    pub attach_type: TriggerAttach,
    pub commands: String,
    pub flags: Vec<TriggerEvent>,
    pub arg_list: Vec<String>,
    pub num_args: i32,
}

/// What kind of entity a trigger can attach to. Mirrors the
/// `ScriptType` enum in the schema; lifted into the world layer so
/// catalogs / dispatchers don't need a `mud-db` import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerAttach {
    Mob,
    Object,
    World,
}

/// Event flag a trigger fires on. Mirrors the `TriggerFlag` Postgres
/// enum verbatim — kept in the same order so `enumsortorder`-style
/// debugging stays consistent across layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerEvent {
    Global,
    Random,
    Command,
    Load,
    Cast,
    Leave,
    Time,
    Speech,
    Act,
    Death,
    Greet,
    GreetAll,
    Entry,
    Receive,
    Fight,
    HitPercent,
    Bribe,
    Memory,
    Door,
    SpeechTo,
    Look,
    Auto,
    Attack,
    Defend,
    Timer,
    Get,
    Drop,
    Give,
    Wear,
    Remove,
    Use,
    Consume,
    Reset,
    Preentry,
    Postentry,
}

/// Catalog of every Lua trigger, plus the per-prototype mob/object
/// and per-room (zone, id) → list of trigger keys. Spawn paths copy
/// the per-proto list onto fresh entities as `AttachedTriggers`;
/// rooms get the same treatment in Pass 2 of the loader.
#[derive(Resource, Debug, Default)]
pub struct TriggerCatalog {
    pub by_key: HashMap<(i32, i32), TriggerDef>,
    /// `(mob_zone, mob_id)` → list of `(trigger_zone, trigger_id)`.
    pub mob_attachments: HashMap<(i32, i32), Vec<(i32, i32)>>,
    /// `(object_zone, object_id)` → list of trigger keys.
    pub object_attachments: HashMap<(i32, i32), Vec<(i32, i32)>>,
    /// `(room_zone, room_id)` → list of trigger keys.
    pub room_attachments: HashMap<(i32, i32), Vec<(i32, i32)>>,
}

/// `LevelDefinition` rows ordered by level. Drives the `level`
/// readout (current XP vs threshold for next level) and is the
/// eventual source of HP/stamina gains for level-ups.
#[derive(Resource, Debug, Default)]
pub struct LevelTable {
    pub rows: Vec<LevelRow>,
}

#[derive(Debug, Clone)]
pub struct LevelRow {
    pub level: i32,
    pub name: Option<String>,
    pub exp_required: i32,
    pub hp_gain: i32,
    pub stamina_gain: i32,
    pub is_immortal: bool,
    /// Permissions granted on reaching this level (B5). Unioned
    /// into the character's `Account.perms` on level-up.
    pub permissions: Vec<mud_db::enums::Permission>,
}

impl LevelTable {
    /// Cumulative XP needed to reach `level`, or `None` if the
    /// level isn't in the table (above max).
    #[must_use]
    pub fn exp_for(&self, level: i32) -> Option<i32> {
        self.rows.iter().find(|r| r.level == level).map(|r| r.exp_required)
    }

    /// Display label for `level` ("Apprentice", "Mage", ...) or
    /// the bare numeric form when no name is set.
    #[must_use]
    pub fn name_for(&self, level: i32) -> String {
        self.rows
            .iter()
            .find(|r| r.level == level)
            .and_then(|r| r.name.clone())
            .unwrap_or_else(|| format!("Level {level}"))
    }

    /// Just the `name` column — `Some("Avatar")` for staff levels
    /// that have an explicit title, `None` for ordinary numeric
    /// levels. Score uses this to render only meaningful titles
    /// (immortal ranks today: Avatar / Demi-God / Lesser God /
    /// Greater God / Implementer / Overlord at levels 100..=105)
    /// instead of "Level 25" boilerplate.
    #[must_use]
    pub fn title_for(&self, level: i32) -> Option<&str> {
        self.rows
            .iter()
            .find(|r| r.level == level)
            .and_then(|r| r.name.as_deref())
    }

    /// Per-level HP / Stamina gains for `level`. Returns `None`
    /// when the level isn't in the table (above max). Score uses
    /// this for the "Next level: +N HP, +M Stamina" preview so
    /// players can plan around upcoming level-ups without running
    /// `level` separately.
    #[must_use]
    pub fn gains_for(&self, level: i32) -> Option<(i32, i32)> {
        self.rows
            .iter()
            .find(|r| r.level == level)
            .map(|r| (r.hp_gain, r.stamina_gain))
    }

    /// Snapshot of the underlying rows. Used by callers that need
    /// to mutate the world while inspecting level data without
    /// holding a `Res<LevelTable>` borrow.
    #[must_use]
    pub fn clone_rows(&self) -> Vec<LevelRow> {
        self.rows.clone()
    }
}

/// Per-circle base recover time in seconds. Index = circle number
/// (1..=14), index 0 is unused / "no circle". Ported verbatim from
/// legacy `spell_mem.cpp:195` (`circle_recover_time[]`). Multiplied
/// by class-and-stat focus rate at cast time to set how long a spent
/// slot stays in cooldown. `Ability.addl_mem_time` (per-spell tax)
/// is added on top in the cast handler.
pub const CIRCLE_RECOVER_TIME: [i32; 15] = [
    0,    // 0 — unused
    30,   // circle 1
    35,   // circle 2
    50,   // circle 3
    65,   // circle 4
    80,   // circle 5
    95,   // circle 6
    130,  // circle 7
    145,  // circle 8
    165,  // circle 9
    210,  // circle 10
    250,  // circle 11
    290,  // circle 12
    310,  // circle 13
    330,  // circle 14 — extrapolated; legacy table tops out at 12 entries
];

/// Spell-slot tables loaded once at startup. `progression` maps
/// `(level, circle)` → max slot count; `class_circles` maps
/// `class_id` → list of `(circle, min_level)` the class can access;
/// `ability_circle` maps `(class_id, ability_id)` → circle the
/// spell occupies for that class; `ability_cap` maps the same key
/// → that class's `proficiency_cap` for that ability (`practice`
/// uses it to gate proficiency gains).
#[derive(Resource, Debug, Default)]
pub struct SpellSlotData {
    pub progression: HashMap<(i32, i32), i32>,
    pub class_circles: HashMap<i32, Vec<(i32, i32)>>,
    pub ability_circle: HashMap<(i32, i32), i32>,
    pub ability_cap: HashMap<(i32, i32), i32>,
}

/// `ClassSkills` table flattened for runtime lookup. Keyed by
/// `(class_id, ability_id)` → `min_level` so the SKILL invoke gate
/// can answer "can this class use this skill at this level?" in
/// O(1). Cap data lives in a parallel map for `practice`'s gain
/// limiter, mirroring `SpellSlotData.ability_cap`.
#[derive(Resource, Debug, Default)]
pub struct ClassSkillsData {
    pub min_level: HashMap<(i32, i32), i32>,
    pub proficiency_cap: HashMap<(i32, i32), i32>,
}

impl ClassSkillsData {
    /// `Some(min_level)` if `class_id` has a row for `ability_id`,
    /// otherwise `None`. Callers compare against player level to
    /// decide allow / refuse.
    #[must_use]
    pub fn min_level_for(&self, class_id: i32, ability_id: i32) -> Option<i32> {
        self.min_level.get(&(class_id, ability_id)).copied()
    }

    /// Number of distinct rows the class has — useful for the
    /// "no data loaded" defensive branch in the SKILL gate. A class
    /// with zero rows triggers the legacy bypass so a content gap
    /// doesn't lock players out of their kit.
    #[must_use]
    pub fn class_skill_count(&self, class_id: i32) -> usize {
        self.min_level
            .keys()
            .filter(|(c, _)| *c == class_id)
            .count()
    }
}

impl SpellSlotData {
    /// Maximum slots for `class_id` at character `level`, broken
    /// down by circle. Includes only circles whose `min_level <=
    /// level`. Returns `Vec<(circle, slots)>` sorted by circle.
    #[must_use]
    pub fn slots_for(&self, class_id: i32, level: i32) -> Vec<(i32, i32)> {
        let Some(circles) = self.class_circles.get(&class_id) else {
            return Vec::new();
        };
        let mut out: Vec<(i32, i32)> = circles
            .iter()
            .filter(|(_, min)| *min <= level)
            .map(|(c, _)| {
                let slots = self.progression.get(&(level, *c)).copied().unwrap_or(0);
                (*c, slots)
            })
            .collect();
        out.sort_by_key(|(c, _)| *c);
        out
    }
}

/// In-game time. Advances on a tick system that mirrors the legacy
/// `FieryMUD` pulse cadence: ~75 real seconds per game hour, so a
/// real hour is ~48 game hours (≈ 2 game days). Read by Lua
/// `time.hour` etc., the `weather` flavor lines, and any future
/// day/night-gated systems. Stored as i64 for the wall-clock
/// `stamp` (Unix epoch seconds).
#[derive(Resource, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MudClock {
    pub year: i32,
    pub month: i32,
    pub day: i32,
    pub hour: i32,
    /// Wall-clock seconds since UNIX epoch — refreshed on each
    /// `MudClock` advance so `time.stamp` Lua reads are coherent
    /// with the rest of the system.
    pub stamp: i64,
}

impl Default for MudClock {
    fn default() -> Self {
        Self {
            year: 2025,
            month: 1,
            day: 1,
            hour: 12,
            stamp: 0,
        }
    }
}

/// Sixteen-month calendar inherited from `FieryMUD`'s lore — four
/// thematic months per season, 30 days each. Months are 1-indexed
/// in `MudClock.month`; helpers accept that and clamp out-of-range
/// values to the placeholder so persisted snapshots from a future
/// schema bump still render something readable.
const MONTH_NAMES: [&str; 16] = [
    "the Month of Deepwinter",
    "the Month of the Claw",
    "the Month of the Grand Struggle",
    "the Month of the Running",
    "the Month of the Planting",
    "the Month of the Long Day",
    "the Month of the Time of Famine",
    "the Month of the High Sun",
    "the Month of the Ripening",
    "the Month of the Lowering",
    "the Month of the Fade",
    "the Month of the Dying",
    "the Month of the Shadows",
    "the Month of the Great Frost",
    "the Month of the Drawing",
    "the Month of the Long Night",
];

/// Calendar quarter — months 1..=4 are Winter, 5..=8 Spring, etc.
/// Matches the legacy four-seasons-of-four-months layout exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Season {
    Winter,
    Spring,
    Summer,
    Autumn,
}

impl Season {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Winter => "Winter",
            Self::Spring => "Spring",
            Self::Summer => "Summer",
            Self::Autumn => "Autumn",
        }
    }
}

impl MudClock {
    /// Month name for the current `month` field. Out-of-range months
    /// (a hand-edited snapshot, a future bump) get a placeholder
    /// rather than panicking — `time` is a read-only command and a
    /// crash there isn't worth a defensible invariant elsewhere.
    #[must_use]
    pub fn month_name(&self) -> &'static str {
        usize::try_from(self.month - 1)
            .ok()
            .and_then(|idx| MONTH_NAMES.get(idx).copied())
            .unwrap_or("an unknown month")
    }

    /// Calendar quarter for the current `month`. Out-of-range months
    /// fold into the nearest in-range quarter (months ≤ 0 → Winter,
    /// months ≥ 17 → Autumn) so a hand-edited snapshot still renders.
    #[must_use]
    pub fn season(&self) -> Season {
        match self.month {
            5..=8 => Season::Spring,
            9..=12 => Season::Summer,
            13..=16 => Season::Autumn,
            _ => Season::Winter,
        }
    }
}

/// One entry in the in-memory script error log. Push-only; the
/// reader (admin `scripterrors` command) walks the buffer in
/// reverse chronological order. No DB persistence yet — the
/// schema's `script_error_log` table is the eventual home.
#[derive(Debug, Clone)]
pub struct ScriptError {
    pub at: std::time::SystemTime,
    pub trigger_zone: i32,
    pub trigger_id: i32,
    pub trigger_name: String,
    pub event: String,
    pub message: String,
}

/// Ring buffer of recent trigger fire failures. Pushed by the
/// dispatcher; capped at 256 entries so a runaway trigger doesn't
/// blow memory. Drained by `scripterrors`.
#[derive(Resource, Debug, Default)]
pub struct ScriptErrorLog {
    pub entries: std::collections::VecDeque<ScriptError>,
}

impl ScriptErrorLog {
    pub const CAP: usize = 256;
    pub fn push(&mut self, e: ScriptError) {
        if self.entries.len() >= Self::CAP {
            self.entries.pop_front();
        }
        self.entries.push_back(e);
    }
}

/// One captured global-channel utterance — gossip / shout / music
/// / quest. Drives `lastgossips` and friends so a player who just
/// logged in can catch up on the last few channel lines without
/// asking who's around.
#[derive(Debug, Clone)]
pub struct ChannelEntry {
    pub at: std::time::SystemTime,
    /// Lower-case channel kind: `gossip`, `shout`, `music`, etc.
    pub channel: &'static str,
    pub speaker: String,
    pub body: String,
}

/// Bounded ring buffer of recent channel utterances. In-memory
/// only — clears on restart. Capped at `CAP` entries total
/// (across all channels) so a chatty world can't bleed memory.
#[derive(Resource, Debug, Default)]
pub struct ChannelHistory {
    pub entries: std::collections::VecDeque<ChannelEntry>,
}

impl ChannelHistory {
    pub const CAP: usize = 200;

    pub fn push(&mut self, e: ChannelEntry) {
        if self.entries.len() >= Self::CAP {
            self.entries.pop_front();
        }
        self.entries.push_back(e);
    }

    /// Most-recent-first iterator filtered to one channel kind.
    pub fn recent_on<'a>(
        &'a self,
        channel: &'a str,
    ) -> impl Iterator<Item = &'a ChannelEntry> + 'a {
        self.entries
            .iter()
            .rev()
            .filter(move |e| e.channel.eq_ignore_ascii_case(channel))
    }
}

/// Global wiz-lock toggle — when `true`, the login auth path
/// refuses non-staff (`UserRole` < Builder) accounts at the
/// password-verify step. Toggled by the admin `wizlock` command;
/// reset to `false` on server restart so a forgotten lock doesn't
/// outlive the deploy. Implementation note: a Resource (rather
/// than a static `AtomicBool`) so world-state inspectors can read
/// it through the same `World` view the rest of the runtime uses.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct WizLock {
    pub active: bool,
}

/// One entry in the trigger fire history — every dispatched
/// trigger gets a row. Drives the `trighistory <target>` admin
/// command so builders can confirm whether a trigger actually
/// fired (and on the right entity at the right tick) when a
/// script doesn't behave as expected.
#[derive(Debug, Clone)]
pub struct TriggerHistoryEntry {
    pub at: std::time::SystemTime,
    pub tick: u64,
    pub listener: bevy_ecs::entity::Entity,
    pub trigger_zone: i32,
    pub trigger_id: i32,
    pub event: String,
    pub ok: bool,
}

/// Bounded ring buffer of recent trigger fires. Capped at 512 so a
/// chatty TIMER trigger can't bleed memory; in-memory only — clears
/// on restart.
#[derive(Resource, Debug, Default)]
pub struct TriggerHistoryLog {
    pub entries: std::collections::VecDeque<TriggerHistoryEntry>,
}

impl TriggerHistoryLog {
    pub const CAP: usize = 512;
    pub fn push(&mut self, e: TriggerHistoryEntry) {
        if self.entries.len() >= Self::CAP {
            self.entries.pop_front();
        }
        self.entries.push_back(e);
    }
}

/// One pending `run_room_trigger(zone, id)` Lua call. The Lua
/// binding enqueues into `DeferredRoomTriggerFires` instead of
/// firing inline so a trigger body running in coroutine `A`
/// cannot synchronously re-enter another Lua frame on coroutine
/// `B` — mlua's per-`Lua` re-entrancy guard would panic. The
/// mud-server drain pass runs the queue after the current Lua
/// frame unwinds, on the world thread, with a fresh Lua frame.
#[derive(Debug, Clone)]
pub struct DeferredRoomTriggerFire {
    /// Target room composite key `(zone, id)`.
    pub room_zone: i32,
    pub room_id: i32,
    /// The Lua `self` entity at the call site (typically the mob /
    /// object / room whose trigger called `run_room_trigger`).
    /// Used as the `actor` binding when firing the target room's
    /// triggers. `None` is acceptable — the drain falls back to
    /// using the target room as both `self` and `actor`.
    pub caller: Option<Entity>,
}

/// Queue of `run_room_trigger(zone, id)` Lua calls that fired
/// during the current world tick. Drained by mud-server's
/// `lua_coroutine_tick` after the originating Lua frame returns.
/// Empty between drain cycles.
#[derive(Resource, Debug, Default)]
pub struct DeferredRoomTriggerFires {
    pub queue: Vec<DeferredRoomTriggerFire>,
}

/// Queued output produced by Lua trigger bodies. `messages` carries
/// room broadcasts (`room.send` / `room.send_except`); `direct`
/// carries one-to-one lines (`actor.send`). mud-server drains both
/// after each Lua call returns. mud-script writes; mud-server reads,
/// decoupling the scripting host from the network layer.
#[derive(Resource, Debug, Default)]
pub struct LuaOutbox {
    /// `(room, msg, except)` — room broadcasts, optionally skipping
    /// one recipient.
    pub messages: Vec<(Entity, String, Option<Entity>)>,
    /// `(target, msg)` — direct one-to-one delivery to the target's
    /// Connection.
    pub direct: Vec<(Entity, String)>,
    /// `(actor, line)` — queued commands that should be dispatched as
    /// if `actor` had typed `line`. Drained by mud-server after the
    /// current Lua call returns to avoid re-entering the dispatcher
    /// while a trigger body is still executing.
    pub commands: Vec<(Entity, String)>,
}

/// Catalog of every shop, loaded from `Shops` + `ShopItems` at startup.
/// `keeper_index` maps a keeper mob's `(zone, id)` to the
/// `(shop_zone, shop_id)` that fronts it; `by_key` carries the actual
/// definition. Spawn-time mob resets attach a `Shopkeeper` component
/// pointing at the shop's `(zone, id)`.
#[derive(Resource, Debug, Default)]
pub struct ShopCatalog {
    pub by_key: HashMap<(i32, i32), ShopDef>,
    pub keeper_index: HashMap<(i32, i32), (i32, i32)>,
}

/// Catalog of every player class, keyed by `Class.id`. Loaded once at
/// startup; the runtime reads from this when rendering character info
/// (score, who, etc.).
#[derive(Resource, Debug, Default)]
pub struct ClassCatalog {
    pub by_id: HashMap<i32, ClassDef>,
}

#[derive(Debug, Clone)]
pub struct ClassDef {
    pub id: i32,
    /// Display name — usually carries XML-Lite color tags.
    pub name: String,
    pub plain_name: String,
    pub is_subclass: bool,
    pub parent_class_id: Option<i32>,
    /// Long-form builder prose. `None` when not yet authored —
    /// `cmd_info <class>` falls through to the bare identity line.
    pub description: Option<String>,
    /// Hit-dice expression (`"1d8"` / `"1d10"`) used for natural HP
    /// rolls per class. Schema default `"1d8"`.
    pub hit_dice: String,
    /// Primary attribute label (`"STR"`, `"DEX"`, ...). `None` when
    /// the class doesn't designate one.
    pub primary_stat: Option<String>,
    /// Flat HP gain per level layered on `LevelDefinition.hp_gain`.
    /// Schema default `10`.
    pub hp_per_level: i32,
    /// Per-element resistance map distilled from the schema's
    /// `Class.resistances` JSON: keys are `ElementType` variants
    /// the runtime models, unrecognized strings are dropped at
    /// catalog hydration time so combat reads a clean Rust map.
    pub resistances: HashMap<mud_db::enums::ElementType, i32>,
}

/// Catalog of every ability (spell / chant / song / skill) in the game,
/// keyed by lowercased plain name for command-line lookup. The runtime
/// reads this on `cast` / `spells` / etc. The richer detail tables
/// (`AbilityComponent`, `AbilityEffect`, ...) are loaded on demand
/// once a command actually needs them.
#[derive(Resource, Debug, Default)]
pub struct AbilityCatalog {
    pub by_name: HashMap<String, AbilityDef>,
    /// Per-ability human-readable requirement messages, keyed by
    /// `Ability.id`. Sourced from `AbilityRestrictions.requirements`
    /// (each rule object's `message` field). Used for the `requires:`
    /// metadata readout in cast/skill output.
    pub restriction_messages: HashMap<i32, Vec<String>>,
    /// Per-ability raw rule objects (full JSONB blobs from
    /// `AbilityRestrictions.requirements`), keyed by `Ability.id`.
    /// Each rule has at minimum a `type` field; the runtime evaluator
    /// (`check_ability_restrictions` in commands) interprets the
    /// supported subset and falls through silently on unknown types.
    pub restriction_rules: HashMap<i32, Vec<serde_json::Value>>,
    /// Effect mappings each ability applies, in `order`. Sourced from
    /// `AbilityEffect`. Stored as (`effect_id`, `override_params`) so
    /// the casting pipeline can read per-mapping duration / amount /
    /// flag overrides without re-querying. Trigger / chance / condition
    /// are still on demand.
    pub effects_for: HashMap<i32, Vec<(i32, Option<serde_json::Value>)>>,
    /// Per-ability templated message strings (start / success / fail /
    /// wearoff). 383 of 408 abilities have a row. Read by
    /// `invoke_ability` to emit caster/target/room flavor text in
    /// place of the dispatcher's terse defaults.
    pub messages: HashMap<i32, AbilityMessageSet>,
    /// Per-ability target-validation rules. 9 of 408 abilities have
    /// a row today (BACKSTAB, BASH, KICK, etc.). Read by
    /// `invoke_ability` after target resolution to refuse casts that
    /// don't match the schema's valid target list.
    pub targeting: HashMap<i32, TargetingRule>,
    /// Per-ability saving-throw rules. 2 rows in the schema today
    /// (`BASH` FORTITUDE, `TRIP_UP` REFLEX). Read by `invoke_ability`
    /// before effect application; on a successful save the
    /// `on_save_action` branches the dispatcher (`NEGATE` skips
    /// effects, `HALF_DURATION` halves spawned `EffectInstance`
    /// durations).
    pub saves: HashMap<i32, SavingThrow>,
    /// Per-ability multi-element damage breakdown. 32 rows today
    /// across ~16 spells. The damage arm sums each component
    /// `evaluate(formula) * percentage / 100` to derive total
    /// damage when components exist; otherwise falls back to the
    /// single `override_params.amount` path.
    pub damage_components: HashMap<i32, Vec<DamageComponent>>,
    /// Per-ability material reagent list. Each entry binds an
    /// object proto id; `required` makes it a hard precondition
    /// for casting, `consumed` removes one carried instance on
    /// success. `invoke_ability` reads this before effect
    /// application; missing required reagents refuse the cast
    /// with a templated message.
    pub components: HashMap<i32, Vec<AbilityComponentReq>>,
}

/// One row from the schema's `AbilityComponent` table. The
/// `object_id` is the legacy zone-less id; runtime carrier-check
/// matches on `WorldKey.id` regardless of zone, mirroring the
/// scope of the legacy data.
#[derive(Debug, Clone, Copy)]
pub struct AbilityComponentReq {
    pub object_id: i32,
    pub consumed: bool,
    pub required: bool,
}

/// One element of an ability's damage breakdown loaded from
/// `AbilityDamageComponent`. Element is held as a raw text label
/// since the runtime doesn't model per-element resistances yet.
#[derive(Debug, Clone)]
pub struct DamageComponent {
    pub element: String,
    pub damage_formula: String,
    pub percentage: i32,
    pub sequence: i32,
}

/// Per-ability saving-throw rule loaded from the
/// `AbilitySavingThrow` table. `dc_formula` is a string evaluated
/// against the caster's `FormulaCtx`; `on_save_action` is the raw
/// JSON value (string or object) describing what happens on success.
#[derive(Debug, Clone, Default)]
pub struct SavingThrow {
    pub save_type: String,
    pub dc_formula: String,
    pub on_save_action: serde_json::Value,
}

/// Per-ability targeting rule loaded from the `AbilityTargeting`
/// table. Acceptable target types (`ENEMY_PC`, `ENEMY_NPC`, `CORPSE`,
/// etc.) are kept as strings so the runtime can interpret them
/// incrementally — types it doesn't recognize pass silently.
#[derive(Debug, Clone, Default)]
pub struct TargetingRule {
    pub valid_targets: Vec<String>,
    pub scope: String,
    pub max_targets: i32,
    pub require_los: bool,
}

/// Templated message strings for one ability, post-rendering decisions.
/// All fields are optional; missing fields fall through to the runtime's
/// default phrasing. Templates use `{actor.name}` / `{target.name}` and
/// pronoun placeholders (`{actor.he}`, `{target.him}`, `{target.his}`).
/// See `loader::ability_messages` for the source row shape.
#[derive(Debug, Clone, Default)]
pub struct AbilityMessageSet {
    pub start_to_caster: Option<String>,
    pub start_to_victim: Option<String>,
    pub start_to_room: Option<String>,
    pub success_to_caster: Option<String>,
    pub success_to_victim: Option<String>,
    pub success_to_room: Option<String>,
    pub success_to_self: Option<String>,
    pub success_self_room: Option<String>,
    pub fail_to_caster: Option<String>,
    pub fail_to_victim: Option<String>,
    pub fail_to_room: Option<String>,
    pub wearoff_to_target: Option<String>,
    pub wearoff_to_room: Option<String>,
    pub look_message: Option<String>,
}

// Five bool flags mirror schema columns; see mud_db::abilities::AbilityRow.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct AbilityDef {
    pub id: i32,
    /// Display name (may contain XML-Lite color tags).
    pub name: String,
    /// Lowercased plain name — also the key in `by_name`.
    pub plain_name: String,
    pub description: Option<String>,
    pub kind: mud_db::abilities::AbilityKind,
    pub violent: bool,
    pub combat_ok: bool,
    pub in_combat_only: bool,
    pub cast_time_rounds: i32,
    pub cooldown_ms: i32,
    pub is_area: bool,
    /// Schema label for `Ability.minPosition` (e.g. "STANDING" /
    /// "SITTING") — kept verbatim for display.
    pub min_position_label: String,
    /// Numeric rank derived from `min_position_label` for comparison
    /// against `PostureKind::rank`. Schema rank order is
    /// DEAD=1 .. STANDING=9; runtime postures occupy 6..9. Anything ≤ 6
    /// is satisfied by every runtime posture.
    pub min_posture_rank: i32,
    /// `Ability.target_scope` enum value as a string. Drives AOE
    /// dispatch in `invoke_ability_with` — `SINGLE` →
    /// single-target, `ROOM_ENEMIES` / `ROOM_ALLIES` / `ROOM_ALL`
    /// → fan-out via `invoke_ability_aoe`. `SELF` /
    /// `ROOM_ENVIRONMENT` and legacy values (`CHAIN` / `CONE` /
    /// `LINE` / `AREA` / `GROUP`) fall through to single-target
    /// for now.
    pub target_scope: String,
    /// Gates magical mitigation (Ward) at combat pipeline step 5
    /// per `docs/design/combat.md`. `true` engages Ward in
    /// addition to Armor; `false` routes purely through Armor
    /// (mundane fire from a torch swing, applied poison, etc.).
    /// Loaded from `Ability.is_magical`; SPELL / CHANT / SONG
    /// default true, SKILL defaults false. The Ward stat is not
    /// yet split out from AC, so this field is groundwork — the
    /// mitigation routing lands once Ward is its own component.
    pub is_magical: bool,
    /// `SpellSphere` schema value — fire / water / healing /
    /// enchantment / etc. Lowercased for display. Surfaces on
    /// `spells` / `chants` / `songs` / `skills` listings as a
    /// dim parenthetical suffix so players can scan by elemental
    /// affinity. `None` for abilities without a sphere assignment.
    pub sphere: Option<String>,
    /// `ElementType` schema value — fire / cold / holy / etc.
    /// Lowercased for display. Loaded for future damage-affinity
    /// routing (vulnerability / resistance application);
    /// not yet surfaced on player-facing readouts.
    pub damage_type: Option<String>,
}

/// Cached `MobResets` rows the loader ran, keyed by `reset_id`. The
/// respawn tick walks this to decide whether each row needs to refill
/// up to `max_instances`. The room entity is resolved at load time so
/// the tick doesn't need to look it up via `WorldKeyIndex` each pass.
#[derive(Resource, Debug, Default)]
pub struct MobResetCatalog {
    pub entries: Vec<MobResetEntry>,
}

#[derive(Debug, Clone)]
pub struct MobResetEntry {
    pub reset_id: i32,
    pub mob_zone_id: i32,
    pub mob_id: i32,
    pub room_entity: bevy_ecs::prelude::Entity,
    pub max_instances: i32,
}

/// Object resets cached for the respawn tick. Same shape as
/// `MobResetCatalog`: each entry is a top-level (no
/// `parent_content_id`) reset row whose target room and proto are
/// already resolved. Nested-content resets (chest contents) are
/// not refilled — they spawn once at boot and stay.
#[derive(Resource, Debug, Default)]
pub struct ObjectResetCatalog {
    pub entries: Vec<ObjectResetEntry>,
}

#[derive(Debug, Clone)]
pub struct ObjectResetEntry {
    pub reset_id: i32,
    pub object_zone_id: i32,
    pub object_id: i32,
    pub room_entity: bevy_ecs::prelude::Entity,
    pub max_instances: i32,
}

#[derive(Debug, Clone)]
pub struct MobProto {
    pub zone_id: i32,
    pub id: i32,
    pub name: String,
    pub keywords: Vec<String>,
    pub room_description: String,
    /// Long-form description shown by `examine <mob>`. Empty when
    /// the builder didn't author one — spawn-side falls back to
    /// `room_description` for consistency with the legacy.
    pub examine_description: String,
    /// Lower-cased gender string (`male` / `female` / `neutral` /
    /// `non_binary`). Read by Lua `actor.gender` for trigger
    /// bodies that gate on the mob's gender.
    pub gender: String,
    /// Lower-cased race string (`humanoid` / `elf` / `dragon` /
    /// ...). Read by Lua `actor.race` and the gendered-pronoun
    /// helpers for "his/her/its" rendering.
    pub race: String,
    pub level: i32,
    pub alignment: i32,
    pub role: mud_db::enums::MobRole,
    pub hp_dice_num: i32,
    pub hp_dice_size: i32,
    pub hp_dice_bonus: i32,
    pub damage_dice_num: i32,
    pub damage_dice_size: i32,
    pub damage_dice_bonus: i32,
    /// Combat redesign axes — direct mirror of `Mobs` schema columns
    /// per `docs/design/combat.md`. `derived_combat_stats()` folds
    /// these into a `CombatStats` component at spawn time.
    pub accuracy: i32,
    pub evasion: i32,
    pub attack_power: i32,
    pub spell_power: i32,
    pub penetration_flat: i32,
    pub penetration_percent: i32,
    pub armor_rating: i32,
    /// Folded into `CombatStats.armor_pct` together with `armor_rating`
    /// at conversion time (sum clamped to 100). Schema retains both
    /// columns for content-authoring clarity; the runtime conflates
    /// them per the audit's "fold into armor_pct" plan.
    pub damage_reduction_percent: i32,
    pub soak: i32,
    pub hardness: i32,
    pub perception: i32,
    pub concealment: i32,
    /// Per-damage-type / per-effect resistance map. JSON shape:
    /// `{"FIRE": 50, "COLD": 200, "charm": 0, ...}`. Loaded as-is;
    /// downstream consumers read via the resistance lookup helpers.
    pub resistances: serde_json::Value,
    /// Magical mitigation percentage from `Mobs.ward_percent`
    /// (0..=100). Engaged at combat pipeline step 5 when the
    /// damage source is magical (`Ability.is_magical`). Zero on
    /// most ordinary mobs; bossier authors lift it for "this
    /// dragon shrugs off spells" content. Surfaces verbatim onto
    /// the spawned `CombatStats.ward_pct`.
    pub ward_percent: i32,
    /// Coin awarded to the killer (or dropped) on death, in copper.
    /// Mirrors `Mobs.wealth`.
    pub wealth: i64,
    /// FK to `Class.id`; `None` for classless mobs. Used by Lua
    /// trigger `actor.class` field access for class-gated dialogue.
    pub class_id: Option<i32>,
    /// AI behavior flags from `Mobs.behaviors`. Copied onto every
    /// spawn instance as a `MobBehaviors` component. Empty when the
    /// content has no tags for this mob.
    pub behaviors: Vec<mud_db::enums::MobBehavior>,
    /// Wrong-target alignment-penalty marker. `Normal` for the
    /// vast majority of mobs; non-Normal triggers an alignment
    /// hit on the killer in `combat::handle_death`.
    pub protected_kind: mud_db::enums::ProtectedKind,
    /// Service-role flags (banker / shopkeeper / trainer / ...).
    pub professions: Vec<mud_db::enums::MobProfession>,
    /// Body / form size class. Drives bash/drag/mount disparity
    /// gates and the examine flavor ("It is a HUGE creature.").
    /// Spawns onto each instance as a `Sized` component.
    pub size: mud_db::enums::Size,
    /// Vitality category (LIFE / UNDEAD / MAGIC / CELESTIAL /
    /// DEMONIC / ELEMENTAL). Gates holy/unholy ability filters and
    /// surfaces in examine; spawns as a `LifeForceTag` component.
    pub life_force: mud_db::enums::LifeForce,
    /// Natural attack flavor — drives the combat narration verb
    /// ("The wolf bites you."). Spawns as a `NaturalAttackType`
    /// component.
    pub damage_type: mud_db::enums::DamageType,
    /// Movement-point pool capacity (legacy `move` column). Mob's
    /// stamina equivalent for long wanders; zero means "no pool".
    /// Non-zero values surface as a `MovementPoints` component.
    pub move_points: i32,
    /// Posture the mob starts in (STANDING / SITTING / RESTING /
    /// SLEEPING). The loader derives the runtime `Posture(PostureKind)`
    /// from this at spawn time.
    pub default_position: mud_db::enums::Position,
    /// Identity-flag list: what the mob IS (illusion / animated /
    /// mount / aquatic / summoned / pet). Spawns as a `MobTraits`
    /// component; AQUATIC gates wander targets to water sectors,
    /// MOUNT auto-attaches the `Mountable` marker on spawn.
    pub traits: Vec<mud_db::enums::MobTrait>,
    /// Live movement mode at spawn — usually equals
    /// `default_movement_mode`. Surfaces as a `MovementModeTag`
    /// component.
    pub movement_mode: mud_db::enums::MovementMode,
    /// Reset / re-spawn movement mode. Re-applied each respawn so
    /// a flying drake always comes back airborne.
    pub default_movement_mode: mud_db::enums::MovementMode,
}

impl MobProto {
    /// Average-roll HP from the dice expression `NdM+B`: `N*(M+1)/2 + B`,
    /// matching `avg_damage`'s shape. Deterministic — boss mobs spawn
    /// at expected HP rather than the max-roll. Per-instance random
    /// rolls (when content authors mark them) wait on a content
    /// flag; until then this is the stable default.
    #[must_use]
    pub fn rolled_hp(&self) -> i32 {
        let n = self.hp_dice_num;
        let m = self.hp_dice_size;
        let b = self.hp_dice_bonus;
        if n <= 0 || m <= 0 {
            return (b).max(1);
        }
        (n * (m + 1) / 2 + b).max(1)
    }

    /// Average roll for `damage_dice`; gives a stable `dmg_roll` for `CombatStats`.
    #[must_use]
    pub fn avg_damage(&self) -> i32 {
        let n = self.damage_dice_num;
        let m = self.damage_dice_size;
        let b = self.damage_dice_bonus;
        (n * (m + 1) / 2 + b).max(1)
    }

    /// Build a `CombatStats` component from this proto's new combat
    /// fields. Direct mapping with two conflations:
    ///   * `armor_rating + damage_reduction_percent → armor_pct`
    ///     (sum clamped to 100). Schema keeps both for builder
    ///     clarity; runtime collapses them onto the single mitigation
    ///     axis.
    ///   * `soak → armor_flat` (rename only).
    /// `crit_chance` is fixed at 5 (parity with legacy d20==20 → 5%);
    /// promote to a schema column later if balance demands per-mob
    /// crit tuning.
    #[must_use]
    pub fn derived_combat_stats(&self) -> crate::components::CombatStats {
        let armor_pct = self
            .armor_rating
            .saturating_add(self.damage_reduction_percent)
            .clamp(0, 100);
        crate::components::CombatStats {
            accuracy: self.accuracy,
            evasion: self.evasion,
            attack_power: self.attack_power,
            spell_power: self.spell_power,
            crit_chance: 5,
            pen_pct: self.penetration_percent,
            pen_flat: self.penetration_flat,
            armor_pct,
            armor_flat: self.soak,
            ward_pct: self.ward_percent,
            hardness: self.hardness,
            alignment: self.alignment,
        }
    }
}

/// Catalog of social commands ("smile", "bow", "hug" …) loaded from the
/// Social table at startup. Looked up by name when the command dispatcher
/// fails to find a builtin.
#[derive(Resource, Debug, Default)]
pub struct SocialRegistry {
    pub by_name: HashMap<String, SocialDef>,
}

impl SocialRegistry {
    #[must_use] 
    pub fn get(&self, name: &str) -> Option<&SocialDef> {
        self.by_name.get(&name.to_ascii_lowercase())
    }
}

#[derive(Debug, Clone)]
pub struct SocialDef {
    pub name: String,
    pub hide: bool,
    pub char_no_arg: Option<String>,
    pub others_no_arg: Option<String>,
    pub char_found: Option<String>,
    pub others_found: Option<String>,
    pub vict_found: Option<String>,
    pub not_found: Option<String>,
    pub char_auto: Option<String>,
    pub others_auto: Option<String>,
}

/// Coarse temperature band. Climate sets the resting band; weather
/// drift can move ±1 band per tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TempBand {
    Frigid,
    Cold,
    Cool,
    Mild,
    Warm,
    Hot,
    Sweltering,
}

impl TempBand {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Frigid => "frigid",
            Self::Cold => "cold",
            Self::Cool => "cool",
            Self::Mild => "mild",
            Self::Warm => "warm",
            Self::Hot => "hot",
            Self::Sweltering => "sweltering",
        }
    }
}

/// Precipitation/sky state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PrecipKind {
    Clear,
    Cloudy,
    Drizzle,
    Rain,
    Storm,
    Snow,
    Blizzard,
}

impl PrecipKind {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Cloudy => "overcast",
            Self::Drizzle => "drizzling",
            Self::Rain => "raining",
            Self::Storm => "stormy",
            Self::Snow => "snowing",
            Self::Blizzard => "blizzarding",
        }
    }
}

/// Live weather state for one zone. Updated per `weather_tick`;
/// surfaced via the `weather` command and (eventually) outdoor
/// room descriptions.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct WeatherState {
    pub temp: TempBand,
    pub precip: PrecipKind,
}

/// Per-zone weather catalog. Initialized at world load from the
/// Zone's `Climate`; ticked periodically by the runtime. Fully
/// ephemeral — restart re-derives from climate.
#[derive(Resource, Default, Debug)]
pub struct WeatherCatalog {
    pub by_zone: HashMap<i32, WeatherState>,
}

/// Per-entity bag of Lua-trigger-set variables, backed by the
/// `entity_variables` table. Hydrated once at world boot via
/// `mud_db::entity_variables::list_all` (see `loader::load_from_db`);
/// the runtime then services Lua `:setvar` / `:getvar` / `:clearvar`
/// from this in-memory cache without round-tripping the DB.
///
/// Writes mark the `(EntityType, zone, id)` key dirty; a flush tick in
/// mud-server drains the dirty set via `drain_dirty` and persists each
/// entity's bag with `entity_variables::upsert_many`. Keys whose new
/// value is `None` (Lua `clearvar` / `setvar(nil)`) flush as deletes.
///
/// One cache covers all three entity shapes (mob / object / room) so a
/// trigger body that hops across `self` / `actor.room` / a found
/// object can read and write through the same resource without juggling
/// per-kind handles.
#[derive(Resource, Default, Debug)]
pub struct EntityVariableCache {
    /// In-memory bag. Outer key is the entity-identity tuple; inner
    /// `None` value is the tombstone shape — present in the map so
    /// the next flush deletes the row, then dropped from the map
    /// after the flush completes.
    inner: HashMap<(EntityType, i32, i32), HashMap<String, Option<serde_json::Value>>>,
    /// Entities whose bag changed since the last flush — written to
    /// the DB on the next tick. Cleared by `drain_dirty`.
    dirty: HashSet<(EntityType, i32, i32)>,
}

impl EntityVariableCache {
    /// Read one variable. Returns `None` when the entity has no bag,
    /// the key isn't set, or the key was just cleared (tombstone
    /// awaiting flush).
    #[must_use]
    pub fn get(
        &self,
        kind: EntityType,
        zone: i32,
        id: i32,
        key: &str,
    ) -> Option<&serde_json::Value> {
        self.inner.get(&(kind, zone, id))?.get(key)?.as_ref()
    }

    /// Insert or overwrite a single key. Marks the entity dirty so the
    /// next flush tick persists the change. The previous value (if
    /// any) is discarded — callers that need a swap should `get`
    /// first.
    pub fn set(
        &mut self,
        kind: EntityType,
        zone: i32,
        id: i32,
        key: String,
        value: serde_json::Value,
    ) {
        let bag = self.inner.entry((kind, zone, id)).or_default();
        bag.insert(key, Some(value));
        self.dirty.insert((kind, zone, id));
    }

    /// Drop a single key. Marks the entity dirty even when the key
    /// wasn't present in the cache: a `clearvar` issued before the
    /// hydration sees the row still needs to issue a DELETE on the
    /// next flush. Returns `true` if the key had a non-tombstoned
    /// value at call time (useful for Lua return values).
    pub fn clear(&mut self, kind: EntityType, zone: i32, id: i32, key: &str) -> bool {
        let bag = self.inner.entry((kind, zone, id)).or_default();
        let had_value = bag.get(key).is_some_and(Option::is_some);
        bag.insert(key.to_string(), None);
        self.dirty.insert((kind, zone, id));
        had_value
    }

    /// Hydration entry point — called once from the loader per row
    /// returned by `entity_variables::list_all`. Inserts without
    /// marking dirty, so the bag is durable but the flush tick won't
    /// re-write rows that came straight off disk.
    pub fn hydrate(
        &mut self,
        kind: EntityType,
        zone: i32,
        id: i32,
        key: String,
        value: serde_json::Value,
    ) {
        self.inner
            .entry((kind, zone, id))
            .or_default()
            .insert(key, Some(value));
    }

    /// Pull every dirty entity's (sets, clears) so the flush tick can
    /// persist them. Returns `(EntityType, zone, id, sets, clears)`
    /// tuples. After this returns, tombstones are evicted from the
    /// cache — the row has been "scheduled for delete," and any
    /// subsequent `get` would correctly miss.
    ///
    /// Empty `inner` bags left over from a clear-only entity are also
    /// dropped to keep memory bounded.
    #[must_use]
    pub fn drain_dirty(
        &mut self,
    ) -> Vec<(
        EntityType,
        i32,
        i32,
        Vec<(String, serde_json::Value)>,
        Vec<String>,
    )> {
        let mut out = Vec::with_capacity(self.dirty.len());
        let dirty: Vec<_> = self.dirty.drain().collect();
        for key in dirty {
            let Some(bag) = self.inner.get_mut(&key) else {
                continue;
            };
            let mut sets = Vec::new();
            let mut clears = Vec::new();
            // Walk the bag once: collect (k, v) for sets and tombstoned
            // keys for clears. Drop tombstones from the live map now
            // that the flush has them.
            let keys: Vec<String> = bag.keys().cloned().collect();
            for k in keys {
                match bag.get(&k) {
                    Some(Some(v)) => sets.push((k.clone(), v.clone())),
                    Some(None) => {
                        clears.push(k.clone());
                        bag.remove(&k);
                    }
                    None => {}
                }
            }
            if bag.is_empty() {
                self.inner.remove(&key);
            }
            if !sets.is_empty() || !clears.is_empty() {
                out.push((key.0, key.1, key.2, sets, clears));
            }
        }
        out
    }

    /// Diagnostic count of distinct entities currently tracked.
    /// Used by admin readouts; not load-bearing.
    #[must_use]
    pub fn entity_count(&self) -> usize {
        self.inner.len()
    }
}

/// Per-quest, per-character variable cache (parking-lot resolution).
///
/// Mirrors `EntityVariableCache` in shape, but the key is
/// `(character_id, quest_zone, quest_id)` and the bag is a top-level
/// JSON object stored in `CharacterQuests.variables`. The Lua
/// bindings (`quest:getvar` / `quest:setvar` / `quest:clearvar`)
/// hit this cache; a flush tick in mud-server writes dirty quests
/// back to the DB via `mud_db::quests::set_quest_variable`.
///
/// Empty `inner` bags hint that the row exists but has no variables;
/// they're harmless. Tombstone semantics: `None` value means "delete
/// on flush" — same shape as the entity cache so the flush logic
/// is dual-purpose.
#[derive(Resource, Default, Debug)]
pub struct QuestVariableCache {
    inner: HashMap<
        (String, i32, i32),
        HashMap<String, Option<serde_json::Value>>,
    >,
    dirty: HashSet<(String, i32, i32)>,
}

impl QuestVariableCache {
    /// Read one variable from the quest bag. Returns `None` when the
    /// bag is missing, the key is unset, or the key is tombstoned
    /// awaiting flush.
    #[must_use]
    pub fn get(
        &self,
        character_id: &str,
        quest_zone: i32,
        quest_id: i32,
        key: &str,
    ) -> Option<&serde_json::Value> {
        self.inner
            .get(&(character_id.to_string(), quest_zone, quest_id))?
            .get(key)?
            .as_ref()
    }

    /// Insert or overwrite a single key. Marks the quest dirty.
    pub fn set(
        &mut self,
        character_id: String,
        quest_zone: i32,
        quest_id: i32,
        key: String,
        value: serde_json::Value,
    ) {
        let row_key = (character_id, quest_zone, quest_id);
        let bag = self.inner.entry(row_key.clone()).or_default();
        bag.insert(key, Some(value));
        self.dirty.insert(row_key);
    }

    /// Drop a single key. Marks the quest dirty so the next flush
    /// issues a DELETE-equivalent via
    /// `set_quest_variable(..., &Value::Null)`. Returns `true` if
    /// the key had a value at call time.
    pub fn clear(
        &mut self,
        character_id: String,
        quest_zone: i32,
        quest_id: i32,
        key: &str,
    ) -> bool {
        let row_key = (character_id, quest_zone, quest_id);
        let bag = self.inner.entry(row_key.clone()).or_default();
        let had = bag.get(key).is_some_and(Option::is_some);
        bag.insert(key.to_string(), None);
        self.dirty.insert(row_key);
        had
    }

    /// Hydration entry — used by the loader to pre-fill the cache
    /// from `CharacterQuest.variables` so the first `quest:getvar`
    /// for a hydrated quest doesn't have to hit the DB. Inserts
    /// without marking dirty.
    pub fn hydrate(
        &mut self,
        character_id: String,
        quest_zone: i32,
        quest_id: i32,
        key: String,
        value: serde_json::Value,
    ) {
        self.inner
            .entry((character_id, quest_zone, quest_id))
            .or_default()
            .insert(key, Some(value));
    }

    /// Drain dirty quests for the flush tick. Returns
    /// `(character_id, quest_zone, quest_id, sets, clears)`.
    /// Tombstoned keys come back as `clears` and are evicted from
    /// the live map so subsequent `get`s correctly miss.
    #[must_use]
    pub fn drain_dirty(
        &mut self,
    ) -> Vec<(
        String,
        i32,
        i32,
        Vec<(String, serde_json::Value)>,
        Vec<String>,
    )> {
        let mut out = Vec::with_capacity(self.dirty.len());
        let dirty: Vec<_> = self.dirty.drain().collect();
        for key in dirty {
            let Some(bag) = self.inner.get_mut(&key) else {
                continue;
            };
            let mut sets = Vec::new();
            let mut clears = Vec::new();
            let keys: Vec<String> = bag.keys().cloned().collect();
            for k in keys {
                match bag.get(&k) {
                    Some(Some(v)) => sets.push((k.clone(), v.clone())),
                    Some(None) => {
                        clears.push(k.clone());
                        bag.remove(&k);
                    }
                    None => {}
                }
            }
            if bag.is_empty() {
                self.inner.remove(&key);
            }
            if !sets.is_empty() || !clears.is_empty() {
                out.push((key.0, key.1, key.2, sets, clears));
            }
        }
        out
    }

    /// Diagnostic count.
    #[must_use]
    pub fn quest_count(&self) -> usize {
        self.inner.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock_for_month(month: i32) -> MudClock {
        MudClock { year: 1, month, day: 1, hour: 12, stamp: 0 }
    }

    #[test]
    fn month_name_covers_full_calendar() {
        assert_eq!(clock_for_month(1).month_name(), "the Month of Deepwinter");
        assert_eq!(clock_for_month(8).month_name(), "the Month of the High Sun");
        assert_eq!(clock_for_month(16).month_name(), "the Month of the Long Night");
    }

    #[test]
    fn month_name_clamps_out_of_range() {
        assert_eq!(clock_for_month(0).month_name(), "an unknown month");
        assert_eq!(clock_for_month(17).month_name(), "an unknown month");
        assert_eq!(clock_for_month(-3).month_name(), "an unknown month");
    }

    #[test]
    fn season_partitions_calendar_into_quarters() {
        for m in 1..=4 {
            assert_eq!(clock_for_month(m).season(), Season::Winter);
        }
        for m in 5..=8 {
            assert_eq!(clock_for_month(m).season(), Season::Spring);
        }
        for m in 9..=12 {
            assert_eq!(clock_for_month(m).season(), Season::Summer);
        }
        for m in 13..=16 {
            assert_eq!(clock_for_month(m).season(), Season::Autumn);
        }
    }

    // --- SpellSlotData ---

    fn spell_data_two_classes() -> SpellSlotData {
        let mut data = SpellSlotData::default();
        // Class 1 (Sorcerer): circles 1 at lvl 1, 2 at lvl 5.
        data.class_circles.insert(1, vec![(1, 1), (2, 5)]);
        // Class 4 (Warrior): no circles.
        // Slot progression: at lvl 1 you get 1 circle-1 slot;
        //                   at lvl 5 you get 2 circle-1 + 1 circle-2.
        data.progression.insert((1, 1), 1);
        data.progression.insert((5, 1), 2);
        data.progression.insert((5, 2), 1);
        data
    }

    #[test]
    fn slots_for_unknown_class_is_empty() {
        let data = spell_data_two_classes();
        assert_eq!(data.slots_for(99, 5), Vec::<(i32, i32)>::new());
    }

    #[test]
    fn slots_for_filters_by_min_level() {
        let data = spell_data_two_classes();
        // At level 1, only circle 1 is available (circle 2 needs lvl 5).
        assert_eq!(data.slots_for(1, 1), vec![(1, 1)]);
        // At level 5, both circles available.
        assert_eq!(data.slots_for(1, 5), vec![(1, 2), (2, 1)]);
    }

    #[test]
    fn slots_for_classless_warrior_returns_empty() {
        let data = spell_data_two_classes();
        // Warrior has no circles → no slot list.
        assert_eq!(data.slots_for(4, 50), Vec::<(i32, i32)>::new());
    }

    // --- ClassSkillsData ---

    fn class_skills_warrior_only() -> ClassSkillsData {
        let mut data = ClassSkillsData::default();
        // Warrior (class 4) gets BASH (id 5) at level 1, RIPOSTE
        // (id 287) at level 40.
        data.min_level.insert((4, 5), 1);
        data.min_level.insert((4, 287), 40);
        data.proficiency_cap.insert((4, 5), 100);
        data.proficiency_cap.insert((4, 287), 100);
        data
    }

    #[test]
    fn class_skill_count_returns_zero_for_uncovered_class() {
        let data = class_skills_warrior_only();
        // Sorcerer has no rows yet.
        assert_eq!(data.class_skill_count(1), 0);
        // Warrior has 2 rows.
        assert_eq!(data.class_skill_count(4), 2);
    }

    #[test]
    fn min_level_for_returns_none_when_class_lacks_ability() {
        let data = class_skills_warrior_only();
        // Sorcerer can't BASH.
        assert_eq!(data.min_level_for(1, 5), None);
        // Warrior can — at level 1.
        assert_eq!(data.min_level_for(4, 5), Some(1));
        // Warrior eventually learns RIPOSTE — at level 40.
        assert_eq!(data.min_level_for(4, 287), Some(40));
    }

    // ---------------------------------------------------------------
    // EntityVariableCache
    // ---------------------------------------------------------------

    #[test]
    fn entity_var_cache_set_get_roundtrip() {
        let mut c = EntityVariableCache::default();
        c.set(
            EntityType::Mob,
            30,
            1,
            "count".to_string(),
            serde_json::json!(7),
        );
        assert_eq!(c.get(EntityType::Mob, 30, 1, "count"), Some(&serde_json::json!(7)));
        assert_eq!(c.get(EntityType::Mob, 30, 2, "count"), None, "different id");
        assert_eq!(c.get(EntityType::Object, 30, 1, "count"), None, "different type");
    }

    #[test]
    fn entity_var_cache_clear_marks_dirty_and_drains() {
        let mut c = EntityVariableCache::default();
        c.set(EntityType::Room, 10, 5, "state".to_string(), serde_json::json!("alpha"));
        // Drain — should produce one set.
        let drained = c.drain_dirty();
        assert_eq!(drained.len(), 1);
        let (kind, z, id, sets, clears) = &drained[0];
        assert_eq!(*kind, EntityType::Room);
        assert_eq!(*z, 10);
        assert_eq!(*id, 5);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].0, "state");
        assert!(clears.is_empty());
        // The value is still readable post-drain.
        assert_eq!(
            c.get(EntityType::Room, 10, 5, "state"),
            Some(&serde_json::json!("alpha"))
        );
        // Clear it — drain should now report a clear.
        assert!(c.clear(EntityType::Room, 10, 5, "state"));
        let drained = c.drain_dirty();
        assert_eq!(drained.len(), 1);
        let (_, _, _, sets, clears) = &drained[0];
        assert!(sets.is_empty());
        assert_eq!(clears, &vec!["state".to_string()]);
        // After flush the tombstone evaporates.
        assert_eq!(c.get(EntityType::Room, 10, 5, "state"), None);
        assert_eq!(c.entity_count(), 0, "empty bag pruned");
    }

    #[test]
    fn entity_var_cache_hydrate_does_not_dirty() {
        let mut c = EntityVariableCache::default();
        c.hydrate(EntityType::Object, 1, 2, "loaded".to_string(), serde_json::json!(true));
        let drained = c.drain_dirty();
        assert!(drained.is_empty(), "hydrate must not mark dirty");
        assert_eq!(c.get(EntityType::Object, 1, 2, "loaded"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn entity_var_cache_drain_combines_sets_and_clears_for_one_entity() {
        let mut c = EntityVariableCache::default();
        c.set(EntityType::Mob, 7, 7, "a".into(), serde_json::json!(1));
        c.set(EntityType::Mob, 7, 7, "b".into(), serde_json::json!(2));
        c.clear(EntityType::Mob, 7, 7, "c");
        let drained = c.drain_dirty();
        assert_eq!(drained.len(), 1, "one entity, one row");
        let (_, _, _, sets, clears) = &drained[0];
        assert_eq!(sets.len(), 2);
        assert_eq!(clears.len(), 1);
        // Drain a second time with no changes — empty.
        assert!(c.drain_dirty().is_empty());
    }

    // ---------------------------------------------------------------
    // QuestVariableCache
    // ---------------------------------------------------------------

    #[test]
    fn quest_var_cache_set_get_roundtrip() {
        let mut c = QuestVariableCache::default();
        c.set("char-1".into(), 30, 1, "progress".into(), serde_json::json!(5));
        assert_eq!(c.get("char-1", 30, 1, "progress"), Some(&serde_json::json!(5)));
        // Different character / zone / quest id all miss.
        assert!(c.get("char-2", 30, 1, "progress").is_none(), "different char");
        assert!(c.get("char-1", 31, 1, "progress").is_none(), "different zone");
        assert!(c.get("char-1", 30, 2, "progress").is_none(), "different quest");
    }

    #[test]
    fn quest_var_cache_set_marks_dirty_and_drains() {
        let mut c = QuestVariableCache::default();
        c.set("char-1".into(), 30, 1, "stage".into(), serde_json::json!("alpha"));
        let drained = c.drain_dirty();
        assert_eq!(drained.len(), 1);
        let (cid, qz, qid, sets, clears) = &drained[0];
        assert_eq!(cid, "char-1");
        assert_eq!(*qz, 30);
        assert_eq!(*qid, 1);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].0, "stage");
        assert_eq!(sets[0].1, serde_json::json!("alpha"));
        assert!(clears.is_empty());
        // Value still readable post-drain (mirrors entity_var_cache shape).
        assert_eq!(c.get("char-1", 30, 1, "stage"), Some(&serde_json::json!("alpha")));
    }

    #[test]
    fn quest_var_cache_clear_tombstones_until_flush() {
        let mut c = QuestVariableCache::default();
        c.set("char-1".into(), 30, 1, "stage".into(), serde_json::json!("alpha"));
        let _ = c.drain_dirty();
        // Clear → read sees None (the tombstone shadows the prior value);
        // drain produces a `clears` entry; after drain the entry evaporates.
        assert!(c.clear("char-1".into(), 30, 1, "stage"), "had value before clear");
        assert!(c.get("char-1", 30, 1, "stage").is_none(), "tombstone reads as None");
        let drained = c.drain_dirty();
        assert_eq!(drained.len(), 1);
        let (_, _, _, sets, clears) = &drained[0];
        assert!(sets.is_empty());
        assert_eq!(clears, &vec!["stage".to_string()]);
        assert_eq!(c.quest_count(), 0, "empty bag pruned after flush");
    }

    #[test]
    fn quest_var_cache_hydrate_does_not_dirty() {
        let mut c = QuestVariableCache::default();
        c.hydrate("char-1".into(), 30, 1, "preset".into(), serde_json::json!("from-db"));
        assert!(c.drain_dirty().is_empty(), "hydrate must not mark dirty");
        // The hydrated value is still readable through `get`.
        assert_eq!(
            c.get("char-1", 30, 1, "preset"),
            Some(&serde_json::json!("from-db"))
        );
    }

    #[test]
    fn quest_var_cache_drain_combines_sets_and_clears_for_one_quest() {
        let mut c = QuestVariableCache::default();
        c.set("char-1".into(), 30, 1, "a".into(), serde_json::json!(1));
        c.set("char-1".into(), 30, 1, "b".into(), serde_json::json!(2));
        c.clear("char-1".into(), 30, 1, "c");
        let drained = c.drain_dirty();
        assert_eq!(drained.len(), 1, "one quest, one drain row");
        let (_, _, _, sets, clears) = &drained[0];
        assert_eq!(sets.len(), 2);
        assert_eq!(clears.len(), 1);
        assert!(c.drain_dirty().is_empty(), "second drain after no writes is empty");
    }

    // ---- HelpCatalog ----

    fn help_entry(
        id: i32,
        title: &str,
        keywords: &[&str],
        min_level: i32,
    ) -> HelpEntry {
        HelpEntry {
            id,
            title: title.to_string(),
            content: format!("Body of {title}."),
            min_level,
            category: None,
            usage: None,
            duration: None,
            sphere: None,
            keywords: keywords.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn insert_help(cat: &mut HelpCatalog, entry: HelpEntry) {
        for kw in &entry.keywords {
            cat.by_keyword
                .entry(kw.to_ascii_lowercase())
                .or_default()
                .push(entry.id);
        }
        cat.entries.insert(entry.id, entry);
    }

    fn fireball_only() -> HelpCatalog {
        let mut c = HelpCatalog::default();
        insert_help(&mut c, help_entry(1, "Fireball", &["FIREBALL", "FIRE BALL"], 0));
        c
    }

    #[test]
    fn help_lookup_exact_keyword_returns_found() {
        let cat = fireball_only();
        match cat.lookup("fireball", 50) {
            HelpLookup::Found(e) => assert_eq!(e.title, "Fireball"),
            other => panic!("expected Found, got {other:?}"),
        }
        // Case-insensitive.
        match cat.lookup("FIREBALL", 50) {
            HelpLookup::Found(e) => assert_eq!(e.title, "Fireball"),
            other => panic!("expected Found, got {other:?}"),
        }
        // Multi-word keyword also matches.
        match cat.lookup("fire ball", 50) {
            HelpLookup::Found(e) => assert_eq!(e.title, "Fireball"),
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn help_lookup_substring_of_keyword_does_not_match() {
        // "ball" is a substring of FIREBALL but not its own keyword,
        // and "ball" isn't a title prefix of "Fireball" → NotFound.
        let cat = fireball_only();
        assert!(matches!(cat.lookup("ball", 50), HelpLookup::NotFound));
        // "fire" IS a title prefix of "Fireball" → that's the
        // title-prefix fallback, which is allowed.
        match cat.lookup("fire", 50) {
            HelpLookup::Found(e) => assert_eq!(e.title, "Fireball"),
            other => panic!("expected title-prefix Found, got {other:?}"),
        }
    }

    #[test]
    fn help_lookup_min_level_hides_high_level_entries() {
        let mut cat = HelpCatalog::default();
        // Public spell — anyone can read.
        insert_help(&mut cat, help_entry(1, "Fireball", &["FIREBALL"], 0));
        // Staff-only article — players shouldn't even discover it exists.
        insert_help(&mut cat, help_entry(2, "SlayCommand", &["SLAY"], 100));

        // Low-level viewer hits Fireball fine.
        assert!(matches!(cat.lookup("fireball", 1), HelpLookup::Found(_)));
        // Low-level viewer asking for "slay" gets NotFound — the
        // entry is in the keyword index but filtered out by level.
        assert!(matches!(cat.lookup("slay", 1), HelpLookup::NotFound));
        // Staff viewer sees it.
        match cat.lookup("slay", 105) {
            HelpLookup::Found(e) => assert_eq!(e.title, "SlayCommand"),
            other => panic!("expected staff Found, got {other:?}"),
        }
        // visible_count obeys the gate too.
        assert_eq!(cat.visible_count(1), 1);
        assert_eq!(cat.visible_count(105), 2);
    }

    #[test]
    fn help_lookup_multiple_matches_yields_ambiguous() {
        let mut cat = HelpCatalog::default();
        // Two distinct entries share the keyword "FIRE" — builder
        // mistake or intentional gloss + spell collision. The lookup
        // should hand the player the title list to disambiguate.
        insert_help(&mut cat, help_entry(1, "Fireball", &["FIRE"], 0));
        insert_help(&mut cat, help_entry(2, "Fire (Element)", &["FIRE"], 0));
        match cat.lookup("fire", 50) {
            HelpLookup::AmbiguousMatches(titles) => {
                assert_eq!(titles.len(), 2);
                assert!(titles.contains(&"Fireball".to_string()));
                assert!(titles.contains(&"Fire (Element)".to_string()));
            }
            other => panic!("expected AmbiguousMatches, got {other:?}"),
        }
    }

    #[test]
    fn help_lookup_title_prefix_with_multiple_matches_is_ambiguous() {
        let mut cat = HelpCatalog::default();
        insert_help(&mut cat, help_entry(1, "Fireball", &["FIREBALL"], 0));
        insert_help(&mut cat, help_entry(2, "Fireshield", &["FIRESHIELD"], 0));
        // "fire" hits neither keyword exactly; the title-prefix
        // fallback yields both — should surface as Ambiguous.
        match cat.lookup("fire", 50) {
            HelpLookup::AmbiguousMatches(titles) => {
                assert_eq!(titles, vec!["Fireball".to_string(), "Fireshield".to_string()]);
            }
            other => panic!("expected AmbiguousMatches, got {other:?}"),
        }
    }

    #[test]
    fn help_lookup_empty_keyword_returns_not_found() {
        let cat = fireball_only();
        assert!(matches!(cat.lookup("", 50), HelpLookup::NotFound));
        assert!(matches!(cat.lookup("   ", 50), HelpLookup::NotFound));
    }

    #[test]
    fn help_lookup_duplicate_ids_under_same_keyword_resolves_to_single_match() {
        // Pathological case: same entry id pushed twice into the
        // keyword index (defensive — the loader shouldn't, but the
        // table doesn't enforce keyword uniqueness across rows).
        // Same title → treated as a single match, not Ambiguous.
        let mut cat = HelpCatalog::default();
        insert_help(&mut cat, help_entry(1, "Fireball", &["FIREBALL"], 0));
        // Force a second push under the same keyword.
        cat.by_keyword
            .entry("fireball".to_string())
            .or_default()
            .push(1);
        match cat.lookup("fireball", 50) {
            HelpLookup::Found(e) => assert_eq!(e.title, "Fireball"),
            other => panic!("expected Found, got {other:?}"),
        }
    }

    // ---- LiquidCatalog ----

    fn liquid(id: i32, name: &str, alias: &str, color: &str) -> LiquidDef {
        LiquidDef {
            id,
            name: name.to_string(),
            alias: alias.to_string(),
            color_desc: color.to_string(),
            drunk_effect: 0,
            hunger_effect: 0,
            thirst_effect: 10,
            description: None,
        }
    }

    #[test]
    fn liquid_catalog_lookup_by_alias_is_case_insensitive() {
        let mut cat = LiquidCatalog::default();
        cat.insert(liquid(1, "water", "water", "clear"));
        cat.insert(liquid(5, "dark ale", "dark-ale", "dark"));
        assert!(cat.lookup_alias("water").is_some());
        assert!(cat.lookup_alias("WATER").is_some());
        assert_eq!(cat.lookup_alias("dark-ale").unwrap().name, "dark ale");
        assert!(cat.lookup_alias("nonexistent").is_none());
        assert_eq!(cat.len(), 2);
    }

    #[test]
    fn liquid_catalog_lookup_by_id() {
        let mut cat = LiquidCatalog::default();
        cat.insert(liquid(7, "lemonade", "lemonade", "yellow"));
        cat.insert(liquid(8, "firebreather", "firebreather", "green"));
        assert_eq!(cat.lookup_id(7).unwrap().alias, "lemonade");
        assert_eq!(cat.lookup_id(8).unwrap().color_desc, "green");
        assert!(cat.lookup_id(999).is_none());
    }

    #[test]
    fn liquid_catalog_fallback_picks_water_when_present() {
        let mut cat = LiquidCatalog::default();
        cat.insert(liquid(1, "water", "water", "clear"));
        let f = cat.fallback();
        assert_eq!(f.alias, "water");
        assert_eq!(f.id, 1);
    }

    #[test]
    fn liquid_catalog_fallback_synthesizes_when_water_absent() {
        // Empty catalog: still produces a usable LiquidDef so the
        // drink path keeps working on a catalog-less test world.
        let cat = LiquidCatalog::default();
        let f = cat.fallback();
        assert_eq!(f.alias, "water");
        assert_eq!(f.drunk_effect, 0);
        assert_eq!(f.thirst_effect, 10, "fallback quenches like water");
    }

    /// Build a minimal `RaceDef` for tests. Defaults match the
    /// schema (76 stat caps, 100 factors). Callers override only
    /// what they actually exercise.
    fn race_def_for_test(
        race: &str,
        male_height: (i32, i32),
        female_height: (i32, i32),
        male_weight: (i32, i32),
        female_weight: (i32, i32),
        stat_caps: RaceStatCaps,
    ) -> RaceDef {
        RaceDef {
            race: race.to_string(),
            name: race.to_string(),
            plain_name: race.to_string(),
            keywords: String::new(),
            playable: true,
            humanoid: true,
            magical: false,
            race_align: "NEUTRAL".to_string(),
            default_alignment: 0,
            default_size: "MEDIUM".to_string(),
            focus_bonus: 100,
            default_lifeforce: "LIFE".to_string(),
            male_weight_low: male_weight.0,
            male_weight_high: male_weight.1,
            male_height_low: male_height.0,
            male_height_high: male_height.1,
            female_weight_low: female_weight.0,
            female_weight_high: female_weight.1,
            female_height_low: female_height.0,
            female_height_high: female_height.1,
            stat_caps,
            exp_factor: 100,
            hp_factor: 100,
            hit_damage_factor: 100,
            damage_dice_factor: 100,
            copper_factor: 100,
            enter_verb: None,
            leave_verb: None,
            start_room_zone_id: None,
            start_room_id: None,
            resistances: HashMap::new(),
            resistances_raw: serde_json::json!({}),
        }
    }

    #[test]
    fn race_catalog_random_height_lands_in_gender_range() {
        let mut cat = RaceCatalog::default();
        cat.by_race.insert(
            "ELF".to_string(),
            race_def_for_test(
                "ELF",
                (60, 72),
                (56, 66),
                (120, 180),
                (100, 150),
                RaceStatCaps::default(),
            ),
        );
        for _ in 0..32 {
            let m = cat.random_height("ELF", "male").expect("male band set");
            assert!(
                (60..=72).contains(&m),
                "male height {m} outside [60,72]",
            );
            let f = cat
                .random_height("ELF", "female")
                .expect("female band set");
            assert!(
                (56..=66).contains(&f),
                "female height {f} outside [56,66]",
            );
        }
    }

    #[test]
    fn race_catalog_random_height_returns_none_for_unauthored() {
        let cat = RaceCatalog::default();
        assert!(cat.random_height("HUMAN", "male").is_none());
        // Race in catalog but both height columns zero → unauthored.
        let mut cat = RaceCatalog::default();
        cat.by_race.insert(
            "GHOST".to_string(),
            race_def_for_test(
                "GHOST",
                (0, 0),
                (0, 0),
                (0, 0),
                (0, 0),
                RaceStatCaps::default(),
            ),
        );
        assert!(cat.random_height("GHOST", "male").is_none());
    }

    #[test]
    fn race_catalog_random_weight_collapses_to_low_when_band_singular() {
        let mut cat = RaceCatalog::default();
        cat.by_race.insert(
            "DWARF".to_string(),
            race_def_for_test(
                "DWARF",
                (50, 60),
                (48, 56),
                (200, 200),
                (180, 180),
                RaceStatCaps::default(),
            ),
        );
        assert_eq!(cat.random_weight("DWARF", "male"), Some(200));
        assert_eq!(cat.random_weight("DWARF", "female"), Some(180));
    }

    #[test]
    fn race_catalog_stat_cap_falls_back_for_unauthored_race() {
        let cat = RaceCatalog::default();
        assert_eq!(cat.stat_cap("UNKNOWN", "strength", 18), 18);
    }

    #[test]
    fn race_catalog_stat_cap_reads_per_attribute_max() {
        let mut cat = RaceCatalog::default();
        cat.by_race.insert(
            "GIANT".to_string(),
            race_def_for_test(
                "GIANT",
                (90, 120),
                (85, 110),
                (300, 500),
                (280, 460),
                RaceStatCaps {
                    strength: 80,
                    dexterity: 60,
                    constitution: 75,
                    intelligence: 50,
                    wisdom: 60,
                    charisma: 60,
                },
            ),
        );
        assert_eq!(cat.stat_cap("GIANT", "str", i32::MAX), 80);
        assert_eq!(cat.stat_cap("GIANT", "dex", i32::MAX), 60);
        assert_eq!(cat.stat_cap("GIANT", "intelligence", i32::MAX), 50);
        // Case-insensitive on the stat label.
        assert_eq!(cat.stat_cap("GIANT", "Wisdom", i32::MAX), 60);
        // Unknown stat token: caller's fallback wins.
        assert_eq!(cat.stat_cap("GIANT", "luck", 42), 42);
    }

    #[test]
    fn parse_resistance_json_drops_unknown_keys() {
        let raw = serde_json::json!({
            "FIRE": 25,
            "ColD": -10,
            "Wibble": 99,
            "MENTAL": 50,
        });
        let map = parse_resistance_json(&raw);
        assert_eq!(map.get(&mud_db::enums::ElementType::Fire), Some(&25));
        assert_eq!(map.get(&mud_db::enums::ElementType::Cold), Some(&-10));
        assert_eq!(map.get(&mud_db::enums::ElementType::Mental), Some(&50));
        assert!(!map
            .keys()
            .any(|k| matches!(k, mud_db::enums::ElementType::Slash)));
    }

    #[test]
    fn parse_resistance_json_handles_null_and_non_object() {
        assert!(parse_resistance_json(&serde_json::json!(null)).is_empty());
        assert!(parse_resistance_json(&serde_json::json!("FIRE")).is_empty());
        assert!(parse_resistance_json(&serde_json::json!([1, 2, 3])).is_empty());
    }
}
