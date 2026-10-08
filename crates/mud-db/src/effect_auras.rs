//! `EffectAura` — flavor sentences shown when someone `look`s at an actor
//! carrying a matching effect (legacy `print_char_spells_to_char`).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectAuraRow {
    pub slug: String,
    /// Labels matched against the originating ability's name and the
    /// effect's flag name.
    pub keys: Vec<String>,
    /// Sentence with `{S}` / `{M}` / `{E}` / `{^S}` / `{^E}` placeholders.
    pub text: String,
    pub needs_detect_magic: bool,
    /// Rows sharing a group show only the first match (by `sort_order`).
    pub exclusive_group: Option<String>,
    /// Inclusive bearer-alignment bounds.
    pub min_alignment: Option<i32>,
    pub max_alignment: Option<i32>,
    pub sort_order: i32,
}

/// Every aura row in display order, for the boot-time `EffectAuraCatalog`.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<EffectAuraRow>> {
    sqlx::query_as!(
        EffectAuraRow,
        r#"
        SELECT
            slug,
            keys AS "keys!: Vec<String>",
            text,
            needs_detect_magic,
            exclusive_group,
            min_alignment,
            max_alignment,
            sort_order
        FROM "EffectAura"
        ORDER BY sort_order, id
        "#
    )
    .fetch_all(pool)
    .await
}
