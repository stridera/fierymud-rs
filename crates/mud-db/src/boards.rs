use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Board {
    pub id: i32,
    /// Short identifier players type — `mortal`, `god`, `quest`, etc.
    pub alias: String,
    /// Display title for the listing header.
    pub title: String,
    /// When true, posts and edits are refused.
    pub locked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardMessage {
    pub id: i32,
    pub board_id: i32,
    /// Character name who posted (legacy stores this as a string;
    /// no FK to Characters).
    pub poster: String,
    pub poster_level: i32,
    pub posted_at: chrono::NaiveDateTime,
    pub subject: String,
    pub content: String,
    pub sticky: bool,
}

pub async fn list_boards(pool: &PgPool) -> sqlx::Result<Vec<Board>> {
    sqlx::query_as!(
        Board,
        r#"
        SELECT id, alias, title, locked
        FROM "Board"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}

pub async fn find_board_by_alias(pool: &PgPool, alias: &str) -> sqlx::Result<Option<Board>> {
    sqlx::query_as!(
        Board,
        r#"
        SELECT id, alias, title, locked
        FROM "Board"
        WHERE LOWER(alias) = LOWER($1)
        LIMIT 1
        "#,
        alias,
    )
    .fetch_optional(pool)
    .await
}

/// Messages on a board, sticky-first then newest first. Suitable for
/// the per-board listing view.
pub async fn messages_for_board(pool: &PgPool, board_id: i32) -> sqlx::Result<Vec<BoardMessage>> {
    sqlx::query_as!(
        BoardMessage,
        r#"
        SELECT
            id,
            board_id,
            poster,
            poster_level AS "poster_level!: i32",
            posted_at AS "posted_at!: chrono::NaiveDateTime",
            subject,
            content,
            sticky
        FROM "BoardMessage"
        WHERE board_id = $1
        ORDER BY sticky DESC, posted_at DESC
        "#,
        board_id,
    )
    .fetch_all(pool)
    .await
}

/// Insert a new board message. Returns the row id. Caller is
/// responsible for any privilege check (the schema's `privileges`
/// JSON field isn't enforced here).
pub async fn post_message(
    pool: &PgPool,
    board_id: i32,
    poster: &str,
    poster_level: i32,
    subject: &str,
    content: &str,
) -> sqlx::Result<i32> {
    let row = sqlx::query!(
        r#"
        INSERT INTO "BoardMessage"
            (board_id, poster, poster_level, posted_at, subject, content, updated_at)
        VALUES ($1, $2, $3, NOW(), $4, $5, NOW())
        RETURNING id
        "#,
        board_id,
        poster,
        poster_level,
        subject,
        content,
    )
    .fetch_one(pool)
    .await?;
    Ok(row.id)
}
