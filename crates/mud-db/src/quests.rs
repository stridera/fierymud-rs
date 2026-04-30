use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterQuestRow {
    pub id: String,
    pub character_id: String,
    pub quest_zone_id: i32,
    pub quest_id: i32,
    pub status: String,
    pub accepted_at: chrono::NaiveDateTime,
    pub completed_at: Option<chrono::NaiveDateTime>,
    pub completion_count: i32,
    /// Quest's display name, joined from `Quest`.
    pub quest_name: String,
    /// Quest's short description (one-liner for the listing).
    pub short_description: Option<String>,
}

/// Active (and recently completed) quests for one character. The
/// `Quest` join supplies the display name + short description so the
/// listing render doesn't need a second query. Sorted with active
/// quests first (`IN_PROGRESS` before `COMPLETED`/`FAILED`/`ABANDONED`),
/// then by `accepted_at` newest-first.
pub async fn list_for_character(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<Vec<CharacterQuestRow>> {
    sqlx::query_as!(
        CharacterQuestRow,
        r#"
        SELECT
            cq.id,
            cq.character_id,
            cq.quest_zone_id,
            cq.quest_id,
            cq.status::text AS "status!: String",
            cq.accepted_at AS "accepted_at!: chrono::NaiveDateTime",
            cq.completed_at AS "completed_at: chrono::NaiveDateTime",
            cq.completion_count AS "completion_count!: i32",
            q.plain_name AS "quest_name!: String",
            q.short_description
        FROM "CharacterQuest" cq
        JOIN "Quest" q
            ON q.zone_id = cq.quest_zone_id AND q.id = cq.quest_id
        WHERE cq.character_id = $1
        ORDER BY
            CASE cq.status::text
                WHEN 'IN_PROGRESS' THEN 0
                WHEN 'COMPLETED'   THEN 1
                ELSE 2
            END,
            cq.accepted_at DESC
        "#,
        character_id,
    )
    .fetch_all(pool)
    .await
}
