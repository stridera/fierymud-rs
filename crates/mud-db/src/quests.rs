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

/// Quest catalog row read by `questinfo` for the per-quest detail view.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct QuestRow {
    pub zone_id: i32,
    pub id: i32,
    pub name: String,
    pub plain_name: String,
    pub description: Option<String>,
    pub short_description: Option<String>,
    pub min_level: i32,
    pub max_level: i32,
    pub repeatable: bool,
    pub shareable: bool,
    pub hidden: bool,
    pub auto_accept: bool,
}

/// Read one Quest row by composite key. Returns None when the row
/// doesn't exist.
pub async fn get_quest(
    pool: &PgPool,
    zone_id: i32,
    id: i32,
) -> sqlx::Result<Option<QuestRow>> {
    sqlx::query_as!(
        QuestRow,
        r#"
        SELECT
            zone_id,
            id,
            name,
            plain_name,
            description,
            short_description,
            min_level,
            max_level,
            repeatable,
            shareable,
            hidden,
            auto_accept
        FROM "Quest"
        WHERE zone_id = $1 AND id = $2
        "#,
        zone_id,
        id,
    )
    .fetch_optional(pool)
    .await
}

/// Look up a Quest definition by its `(zone_id, id)` composite. Used
/// by admin tooling that needs to confirm a quest exists before
/// mutating `CharacterQuest`.
pub async fn quest_exists(pool: &PgPool, zone_id: i32, id: i32) -> sqlx::Result<bool> {
    let row = sqlx::query!(
        r#"
        SELECT 1 AS found FROM "Quest"
        WHERE zone_id = $1 AND id = $2
        LIMIT 1
        "#,
        zone_id,
        id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// Outcome of a player-initiated `qaccept`. Caller picks the
/// right user-facing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptOutcome {
    Accepted,
    NotFound,
    Hidden,
    LevelTooLow(i32),
    LevelTooHigh(i32),
    AlreadyInProgress,
    AlreadyCompletedNonRepeatable,
    PrerequisiteIncomplete { zone: i32, id: i32 },
}

/// Player-initiated quest acceptance. Validates level + prereqs +
/// non-duplicate before inserting the `IN_PROGRESS` row.
pub async fn accept_for_player(
    pool: &PgPool,
    character_id: &str,
    character_level: i32,
    zone_id: i32,
    quest_id: i32,
) -> sqlx::Result<AcceptOutcome> {
    // Quest existence + level / hidden gates.
    let Some(quest) = get_quest(pool, zone_id, quest_id).await? else {
        return Ok(AcceptOutcome::NotFound);
    };
    if quest.hidden {
        return Ok(AcceptOutcome::Hidden);
    }
    if character_level < quest.min_level {
        return Ok(AcceptOutcome::LevelTooLow(quest.min_level));
    }
    if quest.max_level > 0 && character_level > quest.max_level {
        return Ok(AcceptOutcome::LevelTooHigh(quest.max_level));
    }
    // Existing CharacterQuest row gates.
    let existing = sqlx::query!(
        r#"
        SELECT status::text AS "status!: String"
        FROM "CharacterQuest"
        WHERE character_id = $1 AND quest_zone_id = $2 AND quest_id = $3
        "#,
        character_id,
        zone_id,
        quest_id,
    )
    .fetch_optional(pool)
    .await?;
    if let Some(row) = existing {
        if row.status == "IN_PROGRESS" {
            return Ok(AcceptOutcome::AlreadyInProgress);
        }
        if row.status == "COMPLETED" && !quest.repeatable {
            return Ok(AcceptOutcome::AlreadyCompletedNonRepeatable);
        }
    }
    // Prerequisite check: every required prereq must have a
    // COMPLETED CharacterQuest row.
    let prereqs = sqlx::query!(
        r#"
        SELECT prerequisite_quest_zone_id AS pz,
               prerequisite_quest_id AS pid
        FROM "QuestPrerequisite"
        WHERE quest_zone_id = $1
          AND quest_id = $2
          AND require_completion = true
        "#,
        zone_id,
        quest_id,
    )
    .fetch_all(pool)
    .await?;
    for p in &prereqs {
        let done = sqlx::query!(
            r#"
            SELECT 1 AS found
            FROM "CharacterQuest"
            WHERE character_id = $1
              AND quest_zone_id = $2
              AND quest_id = $3
              AND status = 'COMPLETED'::"QuestStatus"
            LIMIT 1
            "#,
            character_id,
            p.pz,
            p.pid,
        )
        .fetch_optional(pool)
        .await?;
        if done.is_none() {
            return Ok(AcceptOutcome::PrerequisiteIncomplete {
                zone: p.pz,
                id: p.pid,
            });
        }
    }
    // All gates passed. Insert (or revive) the row.
    sqlx::query!(
        r#"
        INSERT INTO "CharacterQuest"
            (id, character_id, quest_zone_id, quest_id, status)
        VALUES (gen_random_uuid()::text, $1, $2, $3, 'IN_PROGRESS'::"QuestStatus")
        ON CONFLICT (character_id, quest_zone_id, quest_id)
        DO UPDATE SET
            status = 'IN_PROGRESS'::"QuestStatus",
            accepted_at = NOW(),
            completed_at = NULL,
            current_phase_id = NULL
        "#,
        character_id,
        zone_id,
        quest_id,
    )
    .execute(pool)
    .await?;
    Ok(AcceptOutcome::Accepted)
}

/// Insert a fresh `IN_PROGRESS` `CharacterQuest` row for testing /
/// admin tooling. Skips if the unique `(character_id, zone_id, id)`
/// row already exists. Returns the row id when inserted, None when
/// already present.
pub async fn admin_assign(
    pool: &PgPool,
    character_id: &str,
    zone_id: i32,
    quest_id: i32,
) -> sqlx::Result<Option<String>> {
    let row = sqlx::query!(
        r#"
        INSERT INTO "CharacterQuest"
            (id, character_id, quest_zone_id, quest_id, status)
        VALUES (gen_random_uuid()::text, $1, $2, $3, 'IN_PROGRESS'::"QuestStatus")
        ON CONFLICT (character_id, quest_zone_id, quest_id) DO NOTHING
        RETURNING id
        "#,
        character_id,
        zone_id,
        quest_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.id))
}

/// Set a `CharacterQuest` row's status to `ABANDONED`. Used by the
/// `abandon` command. Doesn't delete the row — keeps the audit
/// trail and respects the `(character_id, quest_zone_id, quest_id)`
/// unique key (so the player can't immediately re-accept the same
/// quest when its row is still around with a non-IN_PROGRESS state).
pub async fn abandon(pool: &PgPool, character_quest_id: &str) -> sqlx::Result<u64> {
    let result = sqlx::query!(
        r#"
        UPDATE "CharacterQuest"
        SET status = 'ABANDONED'::"QuestStatus",
            completed_at = NOW()
        WHERE id = $1 AND status = 'IN_PROGRESS'::"QuestStatus"
        "#,
        character_quest_id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Force a `CharacterQuest` row from `IN_PROGRESS` to `COMPLETED`,
/// stamping `completed_at` and incrementing `completion_count`. Used
/// by admin tooling for testing — real completion comes from quest
/// objective resolution.
pub async fn admin_complete(pool: &PgPool, character_quest_id: &str) -> sqlx::Result<u64> {
    let result = sqlx::query!(
        r#"
        UPDATE "CharacterQuest"
        SET status = 'COMPLETED'::"QuestStatus",
            completed_at = NOW(),
            completion_count = completion_count + 1
        WHERE id = $1 AND status = 'IN_PROGRESS'::"QuestStatus"
        "#,
        character_quest_id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
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
