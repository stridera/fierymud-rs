use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::{Alignment, DamageType, ObjectFlag, ObjectRestriction, ObjectType, WearFlag};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Object {
    pub zone_id: i32,
    pub id: i32,
    pub r#type: ObjectType,
    pub name: String,
    pub keywords: Vec<String>,
    pub room_description: String,
    pub examine_description: Option<String>,
    pub level: i32,
    pub weight: f64,
    pub cost: i32,
    pub wear_flags: Vec<WearFlag>,
    /// Schema's free-form `values` JSONB — type-specific niche fields
    /// (portal destination, light fuel, food values, spell values,
    /// liquid container info). Combat-critical fields (armor_pct,
    /// weapon dice, weapon damage type) live as typed columns below
    /// — fierylib pre-scales them at import time so the runtime
    /// never sees legacy values.
    pub values: serde_json::Value,
    /// Pre-scaled armor mitigation percent (legacy `Objects.values.AC`
    /// × 2). Positive = better mitigation. Zero for non-armor protos
    /// and for armor with no AC. fierylib does the conversion at
    /// import time so the runtime side has no legacy alias path.
    pub armor_pct: i32,
    /// Weapon dice expression (`NdM+B`). Zeros for non-weapons.
    pub weapon_dice_num: i32,
    pub weapon_dice_size: i32,
    pub weapon_dice_bonus: i32,
    /// Weapon damage type (SLASH / PIERCE / CRUSH / etc.). `None`
    /// for non-weapons or weapons without an authored damage type.
    pub weapon_damage_type: Option<DamageType>,
    /// Alignments that CANNOT use this item. Empty for
    /// unrestricted gear. Mirrors `Objects.restricted_alignments`.
    pub restricted_alignments: Vec<Alignment>,
    /// Class ids that CANNOT use this item. FK to `Class.id`.
    /// Empty for unrestricted gear.
    pub restricted_class_ids: Vec<i32>,
    /// Races that CANNOT use this item. Stored as raw enum
    /// labels (HUMAN / ELF / ...) for direct comparison against
    /// `Profile.race` which is also kept as a string.
    pub restricted_races: Vec<String>,
    /// Boolean attribute flags from `Objects.flags` — GLOW / HUM /
    /// INVISIBLE / MAGIC / etc. Read at spawn time and stamped on
    /// the entity as an `ObjectFlags` component; consumers gate on
    /// them via component lookup rather than walking the proto.
    pub flags: Vec<ObjectFlag>,
    /// "Can't do that" restrictions from `Objects.restrictions` —
    /// NO_DROP / NO_TAKE / NO_SELL / etc. Same per-instance
    /// stamping as `flags`; command handlers consult the component
    /// before mutating world state.
    pub restrictions: Vec<ObjectRestriction>,
    /// Lifetime ticker in legacy `MUD hours`. >0 means the item
    /// decays after this many ticks of game time; the runtime
    /// converts to seconds at instance-spawn time. Zero = no
    /// lifetime (lasts until destroyed by other means).
    pub timer: i32,
    /// Decompose-after-timer mode. >0 puts the item into a
    /// post-timer destruction window (legacy ITEM_DECOMPOSING
    /// flag behavior). Read alongside `timer` at spawn time.
    pub decompose_timer: i32,
}

pub async fn list_objects(pool: &PgPool) -> sqlx::Result<Vec<Object>> {
    sqlx::query_as!(
        Object,
        r#"
        SELECT
            zone_id,
            id,
            type AS "type: ObjectType",
            name,
            keywords AS "keywords!: Vec<String>",
            room_description,
            examine_description,
            level,
            weight,
            cost,
            "wearFlags" AS "wear_flags!: Vec<WearFlag>",
            values AS "values!: serde_json::Value",
            armor_pct,
            weapon_dice_num,
            weapon_dice_size,
            weapon_dice_bonus,
            weapon_damage_type AS "weapon_damage_type: DamageType",
            restricted_alignments AS "restricted_alignments!: Vec<Alignment>",
            restricted_class_ids AS "restricted_class_ids!: Vec<i32>",
            restricted_races::text[] AS "restricted_races!: Vec<String>",
            flags AS "flags!: Vec<ObjectFlag>",
            restrictions AS "restrictions!: Vec<ObjectRestriction>",
            timer,
            decompose_timer
        FROM "Objects"
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
