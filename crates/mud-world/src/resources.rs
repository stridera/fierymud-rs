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
    /// (each rule object's `message` field). Real gating uses the
    /// rule type/parameters; this map is just for the metadata
    /// readout today.
    pub restriction_messages: HashMap<i32, Vec<String>>,
    /// Effect mappings each ability applies, in `order`. Sourced from
    /// `AbilityEffect`. Stored as (`effect_id`, `override_params`) so
    /// the casting pipeline can read per-mapping duration / amount /
    /// flag overrides without re-querying. Trigger / chance / condition
    /// are still on demand.
    pub effects_for: HashMap<i32, Vec<(i32, Option<serde_json::Value>)>>,
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
