//! Summon-mount tiers (`"MountType"`). `summonmount` picks the tier a
//! Paladin / Anti-Paladin qualifies for from these rows; builders edit them
//! in Muditor and `fierylib/data/sql/2026-10-13-mount-types.sql` seeds the
//! defaults.

use sqlx::PgPool;

#[derive(Debug, Clone)]
pub struct MountTypeRow {
    pub tier: i32,
    /// `"good"` (Paladin) or `"evil"` (Anti-Paladin).
    pub alignment_side: String,
    pub min_level: i32,
    /// Alignment magnitude: a good summoner needs alignment >= this, an evil
    /// one <= -this.
    pub min_alignment: i32,
    pub mob_zone_id: i32,
    pub mob_id: i32,
    pub hp_bonus: i32,
    pub mv_bonus: i32,
}

/// Every `"MountType"` row.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<MountTypeRow>> {
    sqlx::query_as!(
        MountTypeRow,
        r#"
        SELECT tier, alignment_side, min_level, min_alignment, mob_zone_id, mob_id,
               hp_bonus, mv_bonus
        FROM "MountType"
        ORDER BY alignment_side, tier
        "#
    )
    .fetch_all(pool)
    .await
}
