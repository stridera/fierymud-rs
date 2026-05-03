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

/// All members of a clan, joined against Characters so the
/// `clan` info readout can show names + levels in one query.
pub async fn members_of(
    pool: &PgPool,
    clan_id: i32,
) -> sqlx::Result<Vec<ClanRosterRow>> {
    sqlx::query_as!(
        ClanRosterRow,
        r#"
        SELECT
            cm.character_id AS "character_id!: String",
            cm.rank::text AS "rank!: String",
            c.name AS "name!: String",
            c.level AS "level!: i32"
        FROM clan_member cm
        JOIN "Characters" c ON c.id = cm.character_id
        WHERE cm.clan_id = $1
        ORDER BY
            CASE cm.rank::text
                WHEN 'LEADER' THEN 0
                WHEN 'OFFICER' THEN 1
                WHEN 'MEMBER' THEN 2
                ELSE 3
            END,
            c.level DESC,
            c.name ASC
        "#,
        clan_id,
    )
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClanRosterRow {
    pub character_id: String,
    pub rank: String,
    pub name: String,
    pub level: i32,
}

/// Look up a clan by case-insensitive abbreviation.
pub async fn get_by_abbrev(
    pool: &PgPool,
    abbrev: &str,
) -> sqlx::Result<Option<ClanRow>> {
    sqlx::query_as!(
        ClanRow,
        r#"SELECT id, name, abbrev, motd FROM clan WHERE LOWER(abbrev) = LOWER($1)"#,
        abbrev,
    )
    .fetch_optional(pool)
    .await
}

/// Create a new clan. The unique constraints on name and abbrev
/// are enforced by the schema; on conflict the caller gets an
/// `Err(_)` they can surface as "name/abbrev already taken."
pub async fn create_clan(
    pool: &PgPool,
    name: &str,
    abbrev: &str,
) -> sqlx::Result<i32> {
    let row = sqlx::query!(
        r#"INSERT INTO clan (name, abbrev) VALUES ($1, $2) RETURNING id"#,
        name,
        abbrev,
    )
    .fetch_one(pool)
    .await?;
    Ok(row.id)
}

/// Update a clan's MOTD. Empty string clears it (NULL in DB).
pub async fn set_motd(
    pool: &PgPool,
    clan_id: i32,
    motd: Option<&str>,
) -> sqlx::Result<u64> {
    let res = sqlx::query!(
        r#"UPDATE clan SET motd = $1 WHERE id = $2"#,
        motd,
        clan_id,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Assign a character to a clan with the given rank. Idempotent
/// on `(character_id)` — the schema's PK is `character_id`, so a
/// player already in a clan gets reassigned (or have their rank
/// updated).
pub async fn assign_member(
    pool: &PgPool,
    character_id: &str,
    clan_id: i32,
    rank: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO clan_member (character_id, clan_id, rank)
        VALUES ($1, $2, $3::"ClanRank")
        ON CONFLICT (character_id) DO UPDATE
        SET clan_id = EXCLUDED.clan_id,
            rank = EXCLUDED.rank
        "#,
    )
    .bind(character_id)
    .bind(clan_id)
    .bind(rank)
    .execute(pool)
    .await?;
    Ok(())
}

/// Remove a character from their clan (if any).
pub async fn remove_member(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<u64> {
    let res = sqlx::query!(
        r#"DELETE FROM clan_member WHERE character_id = $1"#,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Disband a clan: drop every membership row, then drop the clan
/// itself. Returns the count of members that were removed (0 when
/// the clan was already empty). Atomically wrapped so a failure
/// after the membership delete leaves a clean state.
pub async fn delete_clan(pool: &PgPool, clan_id: i32) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let members = sqlx::query!(
        r#"DELETE FROM clan_member WHERE clan_id = $1"#,
        clan_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(r#"DELETE FROM clan WHERE id = $1"#, clan_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(members.rows_affected())
}

/// Look up one clan by id. Returns Ok(None) for missing rows.
pub async fn get_clan(pool: &PgPool, clan_id: i32) -> sqlx::Result<Option<ClanRow>> {
    sqlx::query_as!(
        ClanRow,
        r#"SELECT id, name, abbrev, motd FROM clan WHERE id = $1"#,
        clan_id,
    )
    .fetch_optional(pool)
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
