//! Discord guild configuration — channel IDs for the bot's
//! outbound mirrors (gossip / admin / announcements). The schema
//! pins this table to a single row at primary key 1; the loader
//! reads it once at boot and the runtime consults the channel IDs
//! when queueing outbound messages for the Discord bot.
//!
//! The bot itself runs out-of-process (Muditor-side); the Rust
//! runtime just exposes the channel IDs through a `DiscordConfigCatalog`
//! resource so command handlers can decorate their broadcasts with
//! the correct destination tag.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfigRow {
    pub guild_id: String,
    pub gossip_channel_id: Option<String>,
    pub admin_channel_id: Option<String>,
    pub announcement_channel_id: Option<String>,
    pub enabled: bool,
}

/// Read the single `DiscordConfig` row (primary key is always `1`).
/// Returns `None` if the operator hasn't populated the row yet —
/// the runtime treats that the same as `enabled=false`.
pub async fn get(pool: &PgPool) -> sqlx::Result<Option<DiscordConfigRow>> {
    sqlx::query_as!(
        DiscordConfigRow,
        r#"
        SELECT
            guild_id                AS "guild_id!: String",
            gossip_channel_id       AS "gossip_channel_id: String",
            admin_channel_id        AS "admin_channel_id: String",
            announcement_channel_id AS "announcement_channel_id: String",
            enabled                 AS "enabled!: bool"
        FROM discord_config
        WHERE id = 1
        "#,
    )
    .fetch_optional(pool)
    .await
}
