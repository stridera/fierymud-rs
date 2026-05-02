use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::{Alignment, ObjectType, WearFlag};

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
            restricted_alignments AS "restricted_alignments!: Vec<Alignment>"
        FROM "Objects"
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
