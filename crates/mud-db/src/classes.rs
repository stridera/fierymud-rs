//! Player classes — Sorcerer, Cleric, Warrior, Bard, etc. The runtime
//! reads just the basic identity for display; class mechanics
//! (proficiencies, allowed abilities, hit dice, stat bonuses) are
//! loaded on demand once the systems that need them land.
//!
//! Names commonly carry color tags (`<b:magenta>Sorcerer</>`) so the
//! display path renders them like every other XML-Lite text.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassRow {
    pub id: i32,
    pub name: String,
    pub plain_name: String,
    pub is_subclass: bool,
    pub parent_class_id: Option<i32>,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<ClassRow>> {
    sqlx::query_as!(
        ClassRow,
        r#"
        SELECT id, name, plain_name, is_subclass, parent_class_id
        FROM "Class"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
