#![allow(clippy::doc_markdown)]
//! Quest objective progress tracking.
//!
//! Reads in-progress quests and their objectives for a player, and
//! UPSERTs the matching `CharacterQuestObjective` row when a
//! player action satisfies an objective predicate (kill mob,
//! collect item, etc).

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectiveProgressRow {
    pub character_quest_id: String,
    pub quest_zone_id: i32,
    pub quest_id: i32,
    pub phase_id: i32,
    pub objective_id: i32,
    pub required_count: i32,
    pub scope: String,
    pub show_progress: bool,
    pub player_description: String,
    pub current_count: i32,
}

/// Active KILL_MOB objectives for `character_id` whose target mob
/// matches `(mob_zone, mob_id)` and whose row isn't already
/// marked complete. `is_killer` gates SOLO-scoped rows: when the
/// caller is a non-killer party member, we still want PARTY-scoped
/// objectives but skip SOLO ones. The killer always gets both.
pub async fn list_kill_mob_progress(
    pool: &PgPool,
    character_id: &str,
    mob_zone: i32,
    mob_id: i32,
    is_killer: bool,
) -> sqlx::Result<Vec<ObjectiveProgressRow>> {
    sqlx::query_as!(
        ObjectiveProgressRow,
        r#"
        SELECT
            cq.id AS "character_quest_id!: String",
            qo.quest_zone_id AS "quest_zone_id!: i32",
            qo.quest_id AS "quest_id!: i32",
            qo.phase_id AS "phase_id!: i32",
            qo.id AS "objective_id!: i32",
            qo.required_count AS "required_count!: i32",
            qo.scope::text AS "scope!: String",
            qo.show_progress AS "show_progress!: bool",
            qo.player_description AS "player_description!: String",
            COALESCE(cqo.current_count, 0) AS "current_count!: i32"
        FROM "CharacterQuest" cq
        JOIN "QuestObjective" qo
            ON qo.quest_zone_id = cq.quest_zone_id
           AND qo.quest_id = cq.quest_id
        LEFT JOIN "CharacterQuestObjective" cqo
            ON cqo.character_quest_id = cq.id
           AND cqo.quest_zone_id = qo.quest_zone_id
           AND cqo.quest_id = qo.quest_id
           AND cqo.phase_id = qo.phase_id
           AND cqo.objective_id = qo.id
        WHERE cq.character_id = $1
          AND cq.status = 'IN_PROGRESS'::"QuestStatus"
          AND qo.objective_type = 'KILL_MOB'::"QuestObjectiveType"
          AND qo.target_mob_zone_id = $2
          AND qo.target_mob_id = $3
          AND COALESCE(cqo.completed, false) = false
          AND (qo.scope = 'PARTY'::"QuestObjectiveScope" OR $4)
        "#,
        character_id,
        mob_zone,
        mob_id,
        is_killer,
    )
    .fetch_all(pool)
    .await
}

/// Upsert one objective progress row, returning the new
/// `(current_count, completed)`. Caller computes both so the
/// runtime can pick its own threshold semantics; this just
/// writes them.
/// Active COLLECT_ITEM objectives for `character_id` whose target
/// object matches `(object_zone, object_id)`. Same SOLO/PARTY
/// shape — picking up a quest item in a group can satisfy a
/// teammate's PARTY-scoped collect objective.
pub async fn list_collect_item_progress(
    pool: &PgPool,
    character_id: &str,
    object_zone: i32,
    object_id: i32,
    is_collector: bool,
) -> sqlx::Result<Vec<ObjectiveProgressRow>> {
    sqlx::query_as!(
        ObjectiveProgressRow,
        r#"
        SELECT
            cq.id AS "character_quest_id!: String",
            qo.quest_zone_id AS "quest_zone_id!: i32",
            qo.quest_id AS "quest_id!: i32",
            qo.phase_id AS "phase_id!: i32",
            qo.id AS "objective_id!: i32",
            qo.required_count AS "required_count!: i32",
            qo.scope::text AS "scope!: String",
            qo.show_progress AS "show_progress!: bool",
            qo.player_description AS "player_description!: String",
            COALESCE(cqo.current_count, 0) AS "current_count!: i32"
        FROM "CharacterQuest" cq
        JOIN "QuestObjective" qo
            ON qo.quest_zone_id = cq.quest_zone_id
           AND qo.quest_id = cq.quest_id
        LEFT JOIN "CharacterQuestObjective" cqo
            ON cqo.character_quest_id = cq.id
           AND cqo.quest_zone_id = qo.quest_zone_id
           AND cqo.quest_id = qo.quest_id
           AND cqo.phase_id = qo.phase_id
           AND cqo.objective_id = qo.id
        WHERE cq.character_id = $1
          AND cq.status = 'IN_PROGRESS'::"QuestStatus"
          AND qo.objective_type = 'COLLECT_ITEM'::"QuestObjectiveType"
          AND qo.target_object_zone_id = $2
          AND qo.target_object_id = $3
          AND COALESCE(cqo.completed, false) = false
          AND (qo.scope = 'PARTY'::"QuestObjectiveScope" OR $4)
        "#,
        character_id,
        object_zone,
        object_id,
        is_collector,
    )
    .fetch_all(pool)
    .await
}

/// Active TALK_TO_NPC objectives for `character_id` whose target
/// mob matches `(mob_zone, mob_id)`. Same SOLO/PARTY gate as the
/// other types — a scout chatting up the questgiver brings the
/// rest of the party along on PARTY scope.
pub async fn list_talk_to_npc_progress(
    pool: &PgPool,
    character_id: &str,
    mob_zone: i32,
    mob_id: i32,
    is_speaker: bool,
) -> sqlx::Result<Vec<ObjectiveProgressRow>> {
    sqlx::query_as!(
        ObjectiveProgressRow,
        r#"
        SELECT
            cq.id AS "character_quest_id!: String",
            qo.quest_zone_id AS "quest_zone_id!: i32",
            qo.quest_id AS "quest_id!: i32",
            qo.phase_id AS "phase_id!: i32",
            qo.id AS "objective_id!: i32",
            qo.required_count AS "required_count!: i32",
            qo.scope::text AS "scope!: String",
            qo.show_progress AS "show_progress!: bool",
            qo.player_description AS "player_description!: String",
            COALESCE(cqo.current_count, 0) AS "current_count!: i32"
        FROM "CharacterQuest" cq
        JOIN "QuestObjective" qo
            ON qo.quest_zone_id = cq.quest_zone_id
           AND qo.quest_id = cq.quest_id
        LEFT JOIN "CharacterQuestObjective" cqo
            ON cqo.character_quest_id = cq.id
           AND cqo.quest_zone_id = qo.quest_zone_id
           AND cqo.quest_id = qo.quest_id
           AND cqo.phase_id = qo.phase_id
           AND cqo.objective_id = qo.id
        WHERE cq.character_id = $1
          AND cq.status = 'IN_PROGRESS'::"QuestStatus"
          AND qo.objective_type = 'TALK_TO_NPC'::"QuestObjectiveType"
          AND qo.target_mob_zone_id = $2
          AND qo.target_mob_id = $3
          AND COALESCE(cqo.completed, false) = false
          AND (qo.scope = 'PARTY'::"QuestObjectiveScope" OR $4)
        "#,
        character_id,
        mob_zone,
        mob_id,
        is_speaker,
    )
    .fetch_all(pool)
    .await
}

/// Active VISIT_ROOM objectives for `character_id` whose target
/// room matches `(room_zone, room_id)` and whose row isn't
/// already complete. Same SOLO/PARTY gating shape as
/// `list_kill_mob_progress`.
pub async fn list_visit_room_progress(
    pool: &PgPool,
    character_id: &str,
    room_zone: i32,
    room_id: i32,
    is_visitor: bool,
) -> sqlx::Result<Vec<ObjectiveProgressRow>> {
    sqlx::query_as!(
        ObjectiveProgressRow,
        r#"
        SELECT
            cq.id AS "character_quest_id!: String",
            qo.quest_zone_id AS "quest_zone_id!: i32",
            qo.quest_id AS "quest_id!: i32",
            qo.phase_id AS "phase_id!: i32",
            qo.id AS "objective_id!: i32",
            qo.required_count AS "required_count!: i32",
            qo.scope::text AS "scope!: String",
            qo.show_progress AS "show_progress!: bool",
            qo.player_description AS "player_description!: String",
            COALESCE(cqo.current_count, 0) AS "current_count!: i32"
        FROM "CharacterQuest" cq
        JOIN "QuestObjective" qo
            ON qo.quest_zone_id = cq.quest_zone_id
           AND qo.quest_id = cq.quest_id
        LEFT JOIN "CharacterQuestObjective" cqo
            ON cqo.character_quest_id = cq.id
           AND cqo.quest_zone_id = qo.quest_zone_id
           AND cqo.quest_id = qo.quest_id
           AND cqo.phase_id = qo.phase_id
           AND cqo.objective_id = qo.id
        WHERE cq.character_id = $1
          AND cq.status = 'IN_PROGRESS'::"QuestStatus"
          AND qo.objective_type = 'VISIT_ROOM'::"QuestObjectiveType"
          AND qo.target_room_zone_id = $2
          AND qo.target_room_id = $3
          AND COALESCE(cqo.completed, false) = false
          AND (qo.scope = 'PARTY'::"QuestObjectiveScope" OR $4)
        "#,
        character_id,
        room_zone,
        room_id,
        is_visitor,
    )
    .fetch_all(pool)
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn upsert_progress(
    pool: &PgPool,
    character_quest_id: &str,
    quest_zone_id: i32,
    quest_id: i32,
    phase_id: i32,
    objective_id: i32,
    new_count: i32,
    completed: bool,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        INSERT INTO "CharacterQuestObjective"
            (character_quest_id, quest_zone_id, quest_id, phase_id,
             objective_id, current_count, completed,
             completed_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7,
                CASE WHEN $7 THEN NOW() ELSE NULL END)
        ON CONFLICT (character_quest_id, quest_zone_id, quest_id, phase_id, objective_id)
        DO UPDATE SET
            current_count = EXCLUDED.current_count,
            completed     = EXCLUDED.completed,
            completed_at  = CASE
                WHEN EXCLUDED.completed AND "CharacterQuestObjective".completed_at IS NULL
                  THEN NOW()
                ELSE "CharacterQuestObjective".completed_at
            END
        "#,
        character_quest_id,
        quest_zone_id,
        quest_id,
        phase_id,
        objective_id,
        new_count,
        completed,
    )
    .execute(pool)
    .await?;
    Ok(())
}
