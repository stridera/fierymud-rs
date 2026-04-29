//! `ObjectAbilities` junction table reader. Each row binds an
//! ability to a (zone, id) item proto with an optional charge count
//! and per-cast level. `recite` (scrolls), `wave` (wands), `tap`
//! (staves) all read from this.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectAbilityRow {
    pub id: i32,
    pub ability_id: i32,
    pub level: i32,
    pub object_zone_id: i32,
    pub object_id: i32,
    pub charges: Option<i32>,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<ObjectAbilityRow>> {
    sqlx::query_as!(
        ObjectAbilityRow,
        r#"
        SELECT
            id,
            ability_id,
            level,
            object_zone_id,
            object_id,
            charges
        FROM "ObjectAbilities"
        ORDER BY object_zone_id, object_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
