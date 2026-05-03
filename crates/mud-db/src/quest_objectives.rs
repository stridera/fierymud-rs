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

/// Active USE_SKILL objectives for `character_id` whose target
/// ability matches `ability_id`. Same SOLO/PARTY shape.
pub async fn list_use_skill_progress(
    pool: &PgPool,
    character_id: &str,
    ability_id: i32,
    is_caster: bool,
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
          AND qo.objective_type = 'USE_SKILL'::"QuestObjectiveType"
          AND qo.target_ability_id = $2
          AND COALESCE(cqo.completed, false) = false
          AND (qo.scope = 'PARTY'::"QuestObjectiveScope" OR $3)
        "#,
        character_id,
        ability_id,
        is_caster,
    )
    .fetch_all(pool)
    .await
}

/// Active DELIVER_ITEM objectives for `character_id` whose target
/// object matches `(object_zone, object_id)` AND whose deliver-to
/// mob matches `(mob_zone, mob_id)`. Same SOLO/PARTY shape.
pub async fn list_deliver_item_progress(
    pool: &PgPool,
    character_id: &str,
    object_zone: i32,
    object_id: i32,
    deliver_to_zone: i32,
    deliver_to_id: i32,
    is_giver: bool,
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
          AND qo.objective_type = 'DELIVER_ITEM'::"QuestObjectiveType"
          AND qo.target_object_zone_id = $2
          AND qo.target_object_id = $3
          AND qo.deliver_to_mob_zone_id = $4
          AND qo.deliver_to_mob_id = $5
          AND COALESCE(cqo.completed, false) = false
          AND (qo.scope = 'PARTY'::"QuestObjectiveScope" OR $6)
        "#,
        character_id,
        object_zone,
        object_id,
        deliver_to_zone,
        deliver_to_id,
        is_giver,
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

/// One objective + its current progress for the listing view.
/// Fetched via `list_for_quest` after `cmd_quests` shows the
/// per-quest header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectiveListingRow {
    pub phase_id: i32,
    pub phase_name: String,
    pub phase_order: i32,
    pub objective_id: i32,
    pub player_description: String,
    pub required_count: i32,
    pub current_count: i32,
    pub completed: bool,
    pub show_progress: bool,
    pub scope: String,
    pub current_phase_id: Option<i32>,
}

/// Per-objective snapshot for one CharacterQuest. Returns rows
/// across all phases, sorted by phase order then objective id,
/// so the listing render can group by phase.
pub async fn list_for_quest(
    pool: &PgPool,
    character_quest_id: &str,
) -> sqlx::Result<Vec<ObjectiveListingRow>> {
    sqlx::query_as!(
        ObjectiveListingRow,
        r#"
        SELECT
            qo.phase_id AS "phase_id!: i32",
            qp.name AS "phase_name!: String",
            qp."order" AS "phase_order!: i32",
            qo.id AS "objective_id!: i32",
            qo.player_description AS "player_description!: String",
            qo.required_count AS "required_count!: i32",
            COALESCE(cqo.current_count, 0) AS "current_count!: i32",
            COALESCE(cqo.completed, false) AS "completed!: bool",
            qo.show_progress AS "show_progress!: bool",
            qo.scope::text AS "scope!: String",
            cq.current_phase_id AS "current_phase_id: i32"
        FROM "CharacterQuest" cq
        JOIN "QuestObjective" qo
            ON qo.quest_zone_id = cq.quest_zone_id
           AND qo.quest_id = cq.quest_id
        JOIN "QuestPhase" qp
            ON qp.quest_zone_id = qo.quest_zone_id
           AND qp.quest_id = qo.quest_id
           AND qp.id = qo.phase_id
        LEFT JOIN "CharacterQuestObjective" cqo
            ON cqo.character_quest_id = cq.id
           AND cqo.quest_zone_id = qo.quest_zone_id
           AND cqo.quest_id = qo.quest_id
           AND cqo.phase_id = qo.phase_id
           AND cqo.objective_id = qo.id
        WHERE cq.id = $1
        ORDER BY qp."order", qo.id
        "#,
        character_quest_id,
    )
    .fetch_all(pool)
    .await
}

/// One reward row joined for the post-completion grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestRewardRow {
    pub reward_type: String,
    pub amount: Option<i32>,
    pub object_zone_id: Option<i32>,
    pub object_id: Option<i32>,
    pub ability_id: Option<i32>,
    pub quantity: i32,
}

/// All non-choice rewards across every phase of a quest. Choice
/// groups (one-of-N) are filtered out — the runtime grants only
/// unconditional rewards in the auto-complete path; choice
/// rewards wait on a `qreward` selection command (future).
pub async fn list_quest_rewards(
    pool: &PgPool,
    quest_zone_id: i32,
    quest_id: i32,
) -> sqlx::Result<Vec<QuestRewardRow>> {
    sqlx::query_as!(
        QuestRewardRow,
        r#"
        SELECT
            reward_type::text AS "reward_type!: String",
            amount,
            object_zone_id,
            object_id,
            ability_id,
            quantity AS "quantity!: i32"
        FROM "QuestReward"
        WHERE quest_zone_id = $1
          AND quest_id = $2
          AND choice_group IS NULL
        "#,
        quest_zone_id,
        quest_id,
    )
    .fetch_all(pool)
    .await
}

/// Apply EXPERIENCE / GOLD / SKILL_POINTS / ABILITY rewards
/// directly via DB updates. ITEM and HOUSING are skipped — they
/// need world-side spawning that the async path can't do; the
/// completion message tells the player to talk to the questgiver
/// for those.
pub async fn grant_simple_rewards(
    pool: &PgPool,
    character_id: &str,
    rewards: &[QuestRewardRow],
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    for r in rewards {
        match r.reward_type.as_str() {
            "EXPERIENCE" => {
                if let Some(amount) = r.amount {
                    sqlx::query!(
                        r#"UPDATE "Characters" SET experience = experience + $1 WHERE id = $2"#,
                        amount,
                        character_id,
                    )
                    .execute(&mut *tx)
                    .await?;
                }
            }
            "GOLD" => {
                if let Some(amount) = r.amount {
                    sqlx::query!(
                        r#"UPDATE "Characters" SET wealth = wealth + $1 WHERE id = $2"#,
                        i64::from(amount),
                        character_id,
                    )
                    .execute(&mut *tx)
                    .await?;
                }
            }
            "SKILL_POINTS" => {
                if let Some(amount) = r.amount {
                    sqlx::query!(
                        r#"UPDATE "Characters" SET skill_points = skill_points + $1 WHERE id = $2"#,
                        amount,
                        character_id,
                    )
                    .execute(&mut *tx)
                    .await?;
                }
            }
            "ABILITY" => {
                if let Some(ability_id) = r.ability_id {
                    sqlx::query!(
                        r#"
                        INSERT INTO "CharacterAbilities"
                            (character_id, ability_id, proficiency, known)
                        VALUES ($1, $2, 1, true)
                        ON CONFLICT (character_id, ability_id)
                        DO UPDATE SET known = true
                        "#,
                        character_id,
                        ability_id,
                    )
                    .execute(&mut *tx)
                    .await?;
                }
            }
            // ITEM / HOUSING — skipped here; the runtime announces
            // them and expects a turn-in flow.
            _ => {}
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Outcome of the post-completion phase-advance check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseAdvance {
    /// Some objectives in the current phase are still incomplete.
    Pending,
    /// Phase complete — quest moved to the next phase. Carries
    /// the new phase id and its name for the player message.
    Advanced { new_phase_id: i32, name: String },
    /// All phases complete — quest flipped to COMPLETED.
    QuestComplete,
}

/// Decide whether the just-bumped objective finished the current
/// phase, and if so either advance to the next phase or mark the
/// whole quest complete. Idempotent — safe to call after every
/// objective bump; bails early when the phase isn't fully done.
#[allow(clippy::too_many_lines)]
pub async fn try_advance_phase(
    pool: &PgPool,
    character_quest_id: &str,
) -> sqlx::Result<PhaseAdvance> {
    let mut tx = pool.begin().await?;
    // Pull the quest row + its current phase. If current_phase_id
    // is NULL, treat as "first phase" (lowest order).
    let cq = sqlx::query!(
        r#"
        SELECT
            cq.character_id, cq.quest_zone_id, cq.quest_id, cq.current_phase_id,
            cq.status::text AS "status!: String"
        FROM "CharacterQuest" cq
        WHERE cq.id = $1
        "#,
        character_quest_id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(cq) = cq else {
        return Ok(PhaseAdvance::Pending);
    };
    if cq.status != "IN_PROGRESS" {
        return Ok(PhaseAdvance::Pending);
    }
    let current_phase_id = if let Some(id) = cq.current_phase_id {
        id
    } else {
        // First-phase resolve: pick lowest order.
        let row = sqlx::query!(
            r#"
            SELECT id FROM "QuestPhase"
            WHERE quest_zone_id = $1 AND quest_id = $2
            ORDER BY "order" ASC, id ASC
            LIMIT 1
            "#,
            cq.quest_zone_id,
            cq.quest_id,
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(PhaseAdvance::Pending);
        };
        // Stamp the first phase so subsequent bumps see it.
        sqlx::query!(
            r#"UPDATE "CharacterQuest" SET current_phase_id = $1 WHERE id = $2"#,
            row.id,
            character_quest_id,
        )
        .execute(&mut *tx)
        .await?;
        row.id
    };
    // Are all objectives in the current phase marked complete?
    // Note: rows that don't exist in CharacterQuestObjective count
    // as not-complete (LEFT JOIN + COALESCE).
    let pending = sqlx::query!(
        r#"
        SELECT COUNT(*) AS "n!: i64"
        FROM "QuestObjective" qo
        LEFT JOIN "CharacterQuestObjective" cqo
            ON cqo.character_quest_id = $1
           AND cqo.quest_zone_id = qo.quest_zone_id
           AND cqo.quest_id = qo.quest_id
           AND cqo.phase_id = qo.phase_id
           AND cqo.objective_id = qo.id
        WHERE qo.quest_zone_id = $2
          AND qo.quest_id = $3
          AND qo.phase_id = $4
          AND COALESCE(cqo.completed, false) = false
        "#,
        character_quest_id,
        cq.quest_zone_id,
        cq.quest_id,
        current_phase_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    if pending.n > 0 {
        tx.commit().await?;
        return Ok(PhaseAdvance::Pending);
    }
    // Phase complete. Find the next phase by order; tie-break on id.
    let next = sqlx::query!(
        r#"
        SELECT next.id, next.name
        FROM "QuestPhase" cur
        JOIN "QuestPhase" next
          ON next.quest_zone_id = cur.quest_zone_id
         AND next.quest_id = cur.quest_id
         AND (next."order", next.id) > (cur."order", cur.id)
        WHERE cur.quest_zone_id = $1
          AND cur.quest_id = $2
          AND cur.id = $3
        ORDER BY next."order" ASC, next.id ASC
        LIMIT 1
        "#,
        cq.quest_zone_id,
        cq.quest_id,
        current_phase_id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(next) = next {
        sqlx::query!(
            r#"UPDATE "CharacterQuest" SET current_phase_id = $1 WHERE id = $2"#,
            next.id,
            character_quest_id,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(PhaseAdvance::Advanced {
            new_phase_id: next.id,
            name: next.name,
        });
    }
    // No next phase — quest done.
    sqlx::query!(
        r#"
        UPDATE "CharacterQuest"
        SET status = 'COMPLETED'::"QuestStatus",
            completed_at = NOW(),
            completion_count = completion_count + 1
        WHERE id = $1
        "#,
        character_quest_id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(PhaseAdvance::QuestComplete)
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
