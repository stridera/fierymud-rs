use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::{Alignment, ObjectFlag, ObjectRestriction, ObjectType, WearFlag};

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
    /// Schema's free-form `values` JSONB. Per-type interpretation:
    /// weapons carry `{"Hit Dice": {"num": "N", "size": "M",
    /// "bonus": B}, "Damage Type": "...", "Average": ...}`; lights
    /// carry hours; potions carry levels and spell ids; etc. The
    /// runtime reads only what each consumer needs.
    pub values: serde_json::Value,
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
            restricted_alignments AS "restricted_alignments!: Vec<Alignment>",
            restricted_class_ids AS "restricted_class_ids!: Vec<i32>",
            restricted_races::text[] AS "restricted_races!: Vec<String>",
            flags AS "flags!: Vec<ObjectFlag>",
            restrictions AS "restrictions!: Vec<ObjectRestriction>"
        FROM "Objects"
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
