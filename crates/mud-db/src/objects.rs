use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::ObjectType;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Object {
    pub zone_id: i32,
    pub id: i32,
    pub r#type: ObjectType,
    pub name: String,
    pub level: i32,
    pub weight: f64,
    pub cost: i32,
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
            level,
            weight,
            cost
        FROM "Objects"
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
