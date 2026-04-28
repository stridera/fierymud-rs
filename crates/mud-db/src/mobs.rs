use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::MobRole;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mob {
    pub zone_id: i32,
    pub id: i32,
    pub name: String,
    pub level: i32,
    pub alignment: i32,
    pub role: MobRole,
}

pub async fn list_mobs(pool: &PgPool) -> sqlx::Result<Vec<Mob>> {
    sqlx::query_as!(
        Mob,
        r#"
        SELECT
            zone_id,
            id,
            name,
            level,
            alignment,
            role AS "role: MobRole"
        FROM "Mobs"
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
