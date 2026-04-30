use std::collections::HashMap;

use bevy_ecs::prelude::*;

/// Maps composite (zone, id) keys from the schema to live entities in the world.
/// Used by spawn/lookup code to translate DB references to runtime handles.
#[derive(Resource, Debug, Default)]
pub struct WorldKeyIndex {
    pub zones: HashMap<i32, Entity>,
    pub rooms: HashMap<(i32, i32), Entity>,
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
}

impl MobProto {
    /// Max-roll HP from the dice expression `NdM+B`. Deterministic; combat
    /// damage will be rolled per-tick later.
    #[must_use]
    pub fn rolled_hp(&self) -> i32 {
        (self.hp_dice_num * self.hp_dice_size + self.hp_dice_bonus).max(1)
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
