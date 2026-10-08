//! `PlayerCorpses`: a dead player's corpse. The gear lives in the
//! owner's `CharacterItems` rows tagged with `corpse_id` (nesting kept
//! via `container_id`); the purse is the `coins` column (copper, the
//! same unit as `Characters.wealth`). Deleting the corpse row cascades to
//! its items, which is how decay releases them.
//!
//! Every write that moves value between a corpse and a player must land
//! in the *player's* transaction (see `save_inventory_diff` and
//! [`take_coins`]), never as a separate earlier write, so a crash can
//! lose or duplicate nothing.

use sqlx::{PgConnection, PgPool};

/// One persisted corpse, as the boot loader needs it.
#[derive(Debug, Clone)]
pub struct PlayerCorpseRow {
    pub id: i32,
    pub owner_id: String,
    pub owner_name: String,
    pub owner_level: i32,
    pub room_zone_id: i32,
    pub room_id: i32,
    /// Copper left on the corpse.
    pub coins: i64,
    /// Seconds until decay; zero or negative when it has already expired.
    pub remaining_secs: i32,
}

/// Insert the corpse row inside the death transaction and return its id.
/// `decay_secs` is measured from the database clock.
pub async fn insert(
    conn: &mut PgConnection,
    owner_id: &str,
    room_zone_id: i32,
    room_id: i32,
    coins: i64,
    decay_secs: i32,
) -> sqlx::Result<i32> {
    let row = sqlx::query!(
        r#"
        INSERT INTO "PlayerCorpses" (owner_id, room_zone_id, room_id, coins, decay_at)
        VALUES ($1, $2, $3, $4, NOW()::timestamp + $5::int * INTERVAL '1 second')
        RETURNING id
        "#,
        owner_id,
        room_zone_id,
        room_id,
        coins.max(0),
        decay_secs,
    )
    .fetch_one(conn)
    .await?;
    Ok(row.id)
}

/// Every persisted corpse, expired ones included (their
/// `remaining_secs` is `<= 0`): boot restores them and lets the normal
/// decay path release their contents.
pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<PlayerCorpseRow>> {
    sqlx::query_as!(
        PlayerCorpseRow,
        r#"
        SELECT
            pc.id,
            pc.owner_id,
            c.name  AS owner_name,
            c.level AS owner_level,
            pc.room_zone_id,
            pc.room_id,
            pc.coins,
            EXTRACT(EPOCH FROM (pc.decay_at - NOW()::timestamp))::int AS "remaining_secs!"
        FROM "PlayerCorpses" pc
        JOIN "Characters" c ON c.id = pc.owner_id
        ORDER BY pc.id
        "#,
    )
    .fetch_all(pool)
    .await
}

/// Take `amount` copper off a corpse's purse inside the looter's save
/// transaction (their `Characters.wealth` write, with the coins, lands in
/// the same commit). Clamped at zero; a corpse that has already decayed
/// simply matches no row.
pub async fn take_coins(conn: &mut PgConnection, corpse_id: i32, amount: i64) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "PlayerCorpses" SET coins = GREATEST(coins - $2, 0) WHERE id = $1"#,
        corpse_id,
        amount.max(0),
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Record where a dragged corpse now lies.
pub async fn set_room(
    pool: &PgPool,
    corpse_id: i32,
    room_zone_id: i32,
    room_id: i32,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "PlayerCorpses" SET room_zone_id = $2, room_id = $3 WHERE id = $1"#,
        corpse_id,
        room_zone_id,
        room_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete a corpse and (by cascade) every item still inside it, in one
/// statement. Idempotent: a missing row is not an error.
pub async fn delete(pool: &PgPool, corpse_id: i32) -> sqlx::Result<()> {
    sqlx::query!(r#"DELETE FROM "PlayerCorpses" WHERE id = $1"#, corpse_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// [`delete`] inside the caller's transaction: a resurrected player's
/// save retires the corpse in the same commit that re-homes its items
/// and credits its coins.
pub async fn delete_in(conn: &mut PgConnection, corpse_id: i32) -> sqlx::Result<()> {
    sqlx::query!(r#"DELETE FROM "PlayerCorpses" WHERE id = $1"#, corpse_id)
        .execute(conn)
        .await?;
    Ok(())
}
