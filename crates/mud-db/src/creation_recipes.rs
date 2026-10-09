//! `CreationRecipe` — what the creation spells (Minor Creation, Create Food)
//! conjure. Legacy hard-coded the Minor Creation keyword list
//! (`minor_creation_items[]`) and Create Food's per-class zone
//! (`spell_creations`); builders now edit the rows.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreationRecipeRow {
    /// `Ability.plain_name` of the spell the row belongs to.
    pub ability: String,
    /// Word the caster types (matched as an abbreviation); `None` for spells
    /// that take no word.
    pub keyword: Option<String>,
    /// Caster class the row is limited to; `None` applies to every class.
    pub class_id: Option<i32>,
    pub object_zone_id: i32,
    /// Object to create; `None` means "any FOOD object in `object_zone_id`".
    pub object_id: Option<i32>,
}

/// Every recipe, in `id` order (the keyword abbreviation match order), for the
/// boot-time `CreationRecipes` resource.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<CreationRecipeRow>> {
    sqlx::query_as!(
        CreationRecipeRow,
        r#"
        SELECT
            a.plain_name AS ability,
            r.keyword,
            r.class_id,
            r.object_zone_id,
            r.object_id
        FROM "CreationRecipe" r
        JOIN "Ability" a ON a.id = r.ability_id
        ORDER BY r.id
        "#
    )
    .fetch_all(pool)
    .await
}
