//! `ObjectExtraDescriptions` — keyword-addressable flavor text on
//! an object proto. Builders use these to add detail to items
//! ("look pommel" / "look at the inscription on the ring") without
//! modeling the sub-detail as its own object.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectExtra {
    pub object_zone_id: i32,
    pub object_id: i32,
    pub keywords: Vec<String>,
    pub description: String,
}

pub async fn list_extras(pool: &PgPool) -> sqlx::Result<Vec<ObjectExtra>> {
    sqlx::query_as!(
        ObjectExtra,
        r#"
        SELECT
            object_zone_id,
            object_id,
            keywords AS "keywords!: Vec<String>",
            description
        FROM "ObjectExtraDescriptions"
        ORDER BY object_zone_id, object_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
