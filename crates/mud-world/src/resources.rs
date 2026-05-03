use std::collections::HashMap;

use bevy_ecs::prelude::*;

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
#[derive(Resource, Debug, Default)]
pub struct LiquidIndex {
    pub by_name: HashMap<String, i32>,
    pub drunk_effect: HashMap<String, i32>,
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
    /// Weapon damage dice extracted from `Objects.values`'s
    /// `Hit Dice` field (`{"num": "N", "size": "M", "bonus": B}`).
    /// Zeros for non-weapons (or weapons with malformed values).
    /// `avg_damage()` uses these to resolve the formula evaluator's
    /// `weapon_damage` symbol when this proto is the caster's
    /// wielded item.
    pub weapon_dice_num: i32,
    pub weapon_dice_size: i32,
    pub weapon_dice_bonus: i32,
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

    /// Snapshot of the underlying rows. Used by callers that need
    /// to mutate the world while inspecting level data without
    /// holding a `Res<LevelTable>` borrow.
    #[must_use]
    pub fn clone_rows(&self) -> Vec<LevelRow> {
        self.rows.clone()
    }
}

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
    pub level: i32,
    pub alignment: i32,
    pub role: mud_db::enums::MobRole,
    pub hp_dice_num: i32,
    pub hp_dice_size: i32,
    pub hp_dice_bonus: i32,
    pub damage_dice_num: i32,
    pub damage_dice_size: i32,
    pub damage_dice_bonus: i32,
    pub hit_roll: i32,
    pub armor_class: i32,
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
}
