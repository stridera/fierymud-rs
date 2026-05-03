//! Clan / clan-member reads.
//!
//! The runtime hydrates a `ClanMembership` component on each
//! player at login from these queries; `ctell` then broadcasts
//! to every online player with the same `clan_id`. Admin /
//! roster mutation goes through dedicated commands wired to
//! upserts here.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClanRow {
    pub id: i32,
    pub name: String,
    pub abbrev: String,
    pub motd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClanMembershipRow {
    pub character_id: String,
    pub clan_id: i32,
    pub rank: String,
    pub clan_name: String,
    pub clan_abbrev: String,
}

pub async fn list_clans(pool: &PgPool) -> sqlx::Result<Vec<ClanRow>> {
    sqlx::query_as!(
        ClanRow,
        r#"SELECT id, name, abbrev, motd FROM clan ORDER BY id"#,
    )
    .fetch_all(pool)
    .await
}

/// One row per character per (at-most-one) clan. Joined against
/// `Clan` so the runtime gets the abbrev / name in a single
/// query at login.
pub async fn membership_for(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<Option<ClanMembershipRow>> {
    sqlx::query_as!(
        ClanMembershipRow,
        r#"
        SELECT
            cm.character_id AS "character_id!: String",
            cm.clan_id AS "clan_id!: i32",
            cm.rank::text AS "rank!: String",
            c.name AS "clan_name!: String",
            c.abbrev AS "clan_abbrev!: String"
        FROM clan_member cm
        JOIN clan c ON c.id = cm.clan_id
        WHERE cm.character_id = $1
        "#,
        character_id,
    )
    .fetch_optional(pool)
    .await
}
