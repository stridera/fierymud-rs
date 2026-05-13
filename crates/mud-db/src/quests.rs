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
    /// Optional expiry timestamp for time-limited quests
    /// (Wave 4.2). NULL when the quest has no time limit.
    pub expires_at: Option<chrono::NaiveDateTime>,
    /// Per-character per-quest runtime state JSON (Wave 4.10).
    /// Always populated — defaults to `{}` in the schema, so
    /// callers see at minimum `serde_json::Value::Object(empty)`.
    pub variables: serde_json::Value,
}

/// Quest catalog row read by `questinfo` for the per-quest detail view.
///
/// Wave 4 added every column past `auto_accept`: trigger discriminator
/// + per-type FK targets (4.1), time/cooldown gates (4.2), exclusive
/// group (4.3), availability Lua (4.4).
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
    /// QuestTriggerType enum text — `MOB` / `LEVEL` / `ITEM` /
    /// `ROOM` / `SKILL` / `EVENT` / `AUTO` / `MANUAL`.
    pub trigger_type: String,
    pub trigger_mob_zone_id: Option<i32>,
    pub trigger_mob_id: Option<i32>,
    pub trigger_level: Option<i32>,
    pub trigger_item_zone_id: Option<i32>,
    pub trigger_item_id: Option<i32>,
    pub trigger_room_zone_id: Option<i32>,
    pub trigger_room_id: Option<i32>,
    pub trigger_ability_id: Option<i32>,
    pub trigger_event_id: Option<i32>,
    pub time_limit_minutes: Option<i32>,
    pub cooldown_minutes: Option<i32>,
    pub exclusive_group: Option<String>,
    pub availability_requirement: Option<String>,
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
            auto_accept,
            trigger_type::text AS "trigger_type!: String",
            trigger_mob_zone_id,
            trigger_mob_id,
            trigger_level,
            trigger_item_zone_id,
            trigger_item_id,
            trigger_room_zone_id,
            trigger_room_id,
            trigger_ability_id,
            trigger_event_id,
            time_limit_minutes,
            cooldown_minutes,
            exclusive_group,
            availability_requirement
        FROM "Quest"
        WHERE zone_id = $1 AND id = $2
        "#,
        zone_id,
        id,
    )
    .fetch_optional(pool)
    .await
}

/// Look up quests with `trigger_type = LEVEL` whose `trigger_level`
/// equals the given level. Used by the level-up dispatcher (Wave 4.1)
/// to auto-grant any quest the player just qualified for. Returns
/// only `hidden = false` quests by default — admin-only level
/// triggers stay invisible.
pub async fn list_by_trigger_level(
    pool: &PgPool,
    level: i32,
) -> sqlx::Result<Vec<QuestRow>> {
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
            auto_accept,
            trigger_type::text AS "trigger_type!: String",
            trigger_mob_zone_id,
            trigger_mob_id,
            trigger_level,
            trigger_item_zone_id,
            trigger_item_id,
            trigger_room_zone_id,
            trigger_room_id,
            trigger_ability_id,
            trigger_event_id,
            time_limit_minutes,
            cooldown_minutes,
            exclusive_group,
            availability_requirement
        FROM "Quest"
        WHERE trigger_type = 'LEVEL'::"QuestTriggerType"
          AND trigger_level = $1
        "#,
        level,
    )
    .fetch_all(pool)
    .await
}

/// Quests with `trigger_type = ITEM` whose item key matches. Caller
/// filters by hidden / auto-accept / level / cooldown. Used by the
/// pickup path (Wave 4.1).
pub async fn list_by_trigger_item(
    pool: &PgPool,
    item_zone: i32,
    item_id: i32,
) -> sqlx::Result<Vec<QuestRow>> {
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
            auto_accept,
            trigger_type::text AS "trigger_type!: String",
            trigger_mob_zone_id,
            trigger_mob_id,
            trigger_level,
            trigger_item_zone_id,
            trigger_item_id,
            trigger_room_zone_id,
            trigger_room_id,
            trigger_ability_id,
            trigger_event_id,
            time_limit_minutes,
            cooldown_minutes,
            exclusive_group,
            availability_requirement
        FROM "Quest"
        WHERE trigger_type = 'ITEM'::"QuestTriggerType"
          AND trigger_item_zone_id = $1
          AND trigger_item_id = $2
        "#,
        item_zone,
        item_id,
    )
    .fetch_all(pool)
    .await
}

/// Quests with `trigger_type = ROOM` whose room key matches.
pub async fn list_by_trigger_room(
    pool: &PgPool,
    room_zone: i32,
    room_id: i32,
) -> sqlx::Result<Vec<QuestRow>> {
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
            auto_accept,
            trigger_type::text AS "trigger_type!: String",
            trigger_mob_zone_id,
            trigger_mob_id,
            trigger_level,
            trigger_item_zone_id,
            trigger_item_id,
            trigger_room_zone_id,
            trigger_room_id,
            trigger_ability_id,
            trigger_event_id,
            time_limit_minutes,
            cooldown_minutes,
            exclusive_group,
            availability_requirement
        FROM "Quest"
        WHERE trigger_type = 'ROOM'::"QuestTriggerType"
          AND trigger_room_zone_id = $1
          AND trigger_room_id = $2
        "#,
        room_zone,
        room_id,
    )
    .fetch_all(pool)
    .await
}

/// Quests with `trigger_type = SKILL` whose ability target matches.
pub async fn list_by_trigger_ability(
    pool: &PgPool,
    ability_id: i32,
) -> sqlx::Result<Vec<QuestRow>> {
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
            auto_accept,
            trigger_type::text AS "trigger_type!: String",
            trigger_mob_zone_id,
            trigger_mob_id,
            trigger_level,
            trigger_item_zone_id,
            trigger_item_id,
            trigger_room_zone_id,
            trigger_room_id,
            trigger_ability_id,
            trigger_event_id,
            time_limit_minutes,
            cooldown_minutes,
            exclusive_group,
            availability_requirement
        FROM "Quest"
        WHERE trigger_type = 'SKILL'::"QuestTriggerType"
          AND trigger_ability_id = $1
        "#,
        ability_id,
    )
    .fetch_all(pool)
    .await
}

/// Quests with `trigger_type = EVENT` whose event id matches.
pub async fn list_by_trigger_event(
    pool: &PgPool,
    event_id: i32,
) -> sqlx::Result<Vec<QuestRow>> {
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
            auto_accept,
            trigger_type::text AS "trigger_type!: String",
            trigger_mob_zone_id,
            trigger_mob_id,
            trigger_level,
            trigger_item_zone_id,
            trigger_item_id,
            trigger_room_zone_id,
            trigger_room_id,
            trigger_ability_id,
            trigger_event_id,
            time_limit_minutes,
            cooldown_minutes,
            exclusive_group,
            availability_requirement
        FROM "Quest"
        WHERE trigger_type = 'EVENT'::"QuestTriggerType"
          AND trigger_event_id = $1
        "#,
        event_id,
    )
    .fetch_all(pool)
    .await
}

/// Quests with `trigger_type = AUTO` — granted at character creation
/// (Wave 4.1).
pub async fn list_auto_trigger(pool: &PgPool) -> sqlx::Result<Vec<QuestRow>> {
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
            auto_accept,
            trigger_type::text AS "trigger_type!: String",
            trigger_mob_zone_id,
            trigger_mob_id,
            trigger_level,
            trigger_item_zone_id,
            trigger_item_id,
            trigger_room_zone_id,
            trigger_room_id,
            trigger_ability_id,
            trigger_event_id,
            time_limit_minutes,
            cooldown_minutes,
            exclusive_group,
            availability_requirement
        FROM "Quest"
        WHERE trigger_type = 'AUTO'::"QuestTriggerType"
        "#,
    )
    .fetch_all(pool)
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
    /// Mutually-exclusive group already has a non-abandoned member
    /// (Wave 4.3). The held quest's `(zone, id)` is reported so the
    /// command layer can name it.
    ExclusiveGroupConflict {
        group: String,
        held_zone: i32,
        held_id: i32,
    },
    /// Cooldown after last completion has not elapsed (Wave 4.2).
    /// `remaining_secs` is what to surface to the player.
    Cooldown { remaining_secs: i64 },
    /// `availabilityRequirement` Lua returned non-truthy
    /// (Wave 4.4). String is the raw expression for builder
    /// debugging — players see a fixed "you can't take that quest"
    /// line.
    RequirementNotMet { expression: String },
}

/// Player-initiated quest acceptance. Validates level + prereqs +
/// non-duplicate + cooldown + exclusive-group before inserting the
/// `IN_PROGRESS` row. The availability-Lua check (Wave 4.4) is left
/// to the caller (command layer) because it needs world access.
///
/// On success, stamps `expires_at = NOW() + time_limit_minutes`
/// when the quest has a time limit (Wave 4.2). Existing rows are
/// revived in-place: status flips back to IN_PROGRESS, accepted_at
/// gets a fresh stamp, `variables` resets to `{}`.
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
    // Existing CharacterQuest row gates + cooldown.
    let existing = sqlx::query!(
        r#"
        SELECT
            status::text AS "status!: String",
            completed_at
        FROM "CharacterQuest"
        WHERE character_id = $1 AND quest_zone_id = $2 AND quest_id = $3
        "#,
        character_id,
        zone_id,
        quest_id,
    )
    .fetch_optional(pool)
    .await?;
    if let Some(row) = &existing {
        if row.status == "IN_PROGRESS" {
            return Ok(AcceptOutcome::AlreadyInProgress);
        }
        if row.status == "COMPLETED" && !quest.repeatable {
            return Ok(AcceptOutcome::AlreadyCompletedNonRepeatable);
        }
        // Cooldown gate (Wave 4.2): only applies to repeatable
        // quests whose previous run is COMPLETED.
        if row.status == "COMPLETED"
            && let (Some(cd), Some(done_at)) = (quest.cooldown_minutes, row.completed_at)
        {
            let next_allowed = done_at + chrono::Duration::minutes(i64::from(cd));
            let now = chrono::Utc::now().naive_utc();
            if now < next_allowed {
                let remaining_secs = (next_allowed - now).num_seconds().max(0);
                return Ok(AcceptOutcome::Cooldown { remaining_secs });
            }
        }
    }
    // Exclusive-group gate (Wave 4.3): if this quest carries a
    // non-null exclusive_group, refuse when ANY other quest in
    // that group is held by the character with status != ABANDONED.
    if let Some(group) = &quest.exclusive_group {
        let conflict = sqlx::query!(
            r#"
            SELECT cq.quest_zone_id AS held_zone,
                   cq.quest_id AS held_id
            FROM "CharacterQuest" cq
            JOIN "Quest" q
              ON q.zone_id = cq.quest_zone_id AND q.id = cq.quest_id
            WHERE cq.character_id = $1
              AND q.exclusive_group = $2
              AND cq.status != 'ABANDONED'::"QuestStatus"
              AND NOT (cq.quest_zone_id = $3 AND cq.quest_id = $4)
            LIMIT 1
            "#,
            character_id,
            group,
            zone_id,
            quest_id,
        )
        .fetch_optional(pool)
        .await?;
        if let Some(c) = conflict {
            return Ok(AcceptOutcome::ExclusiveGroupConflict {
                group: group.clone(),
                held_zone: c.held_zone,
                held_id: c.held_id,
            });
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
    // All gates passed. Insert (or revive) the row, stamping
    // expires_at when the quest has a time limit.
    let expires_at: Option<chrono::NaiveDateTime> = quest
        .time_limit_minutes
        .map(|mins| chrono::Utc::now().naive_utc() + chrono::Duration::minutes(i64::from(mins)));
    sqlx::query!(
        r#"
        INSERT INTO "CharacterQuest"
            (id, character_id, quest_zone_id, quest_id, status, expires_at, variables)
        VALUES (gen_random_uuid()::text, $1, $2, $3, 'IN_PROGRESS'::"QuestStatus", $4, '{}'::jsonb)
        ON CONFLICT (character_id, quest_zone_id, quest_id)
        DO UPDATE SET
            status = 'IN_PROGRESS'::"QuestStatus",
            accepted_at = NOW(),
            completed_at = NULL,
            current_phase_id = NULL,
            expires_at = EXCLUDED.expires_at,
            variables = '{}'::jsonb
        "#,
        character_id,
        zone_id,
        quest_id,
        expires_at,
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
            q.short_description,
            cq.expires_at AS "expires_at: chrono::NaiveDateTime",
            cq.variables AS "variables!: serde_json::Value"
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

/// Time-limit sweeper (Wave 4.2). Flips every `IN_PROGRESS` row whose
/// `expires_at` is now in the past to `FAILED`. Returns the affected
/// rows so the caller can notify online players. The ECS-side tick
/// wraps this with a notification loop.
#[derive(Debug, Clone)]
pub struct ExpiredQuest {
    pub character_id: String,
    pub character_quest_id: String,
    pub quest_zone_id: i32,
    pub quest_id: i32,
    pub quest_name: String,
}

pub async fn fail_expired_quests(pool: &PgPool) -> sqlx::Result<Vec<ExpiredQuest>> {
    let rows = sqlx::query!(
        r#"
        UPDATE "CharacterQuest" cq
        SET status = 'FAILED'::"QuestStatus",
            completed_at = NOW()
        FROM "Quest" q
        WHERE q.zone_id = cq.quest_zone_id
          AND q.id = cq.quest_id
          AND cq.status = 'IN_PROGRESS'::"QuestStatus"
          AND cq.expires_at IS NOT NULL
          AND cq.expires_at <= NOW()
        RETURNING
            cq.character_id AS "character_id!: String",
            cq.id AS "character_quest_id!: String",
            cq.quest_zone_id AS "quest_zone_id!: i32",
            cq.quest_id AS "quest_id!: i32",
            q.plain_name AS "quest_name!: String"
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ExpiredQuest {
            character_id: r.character_id,
            character_quest_id: r.character_quest_id,
            quest_zone_id: r.quest_zone_id,
            quest_id: r.quest_id,
            quest_name: r.quest_name,
        })
        .collect())
}

/// Per-character per-quest variable JSON read (Wave 4.10). Returns
/// the raw JSON object — the `quest:getvar` Lua binding navigates
/// it. Returns `None` when the CharacterQuest row is missing.
pub async fn get_quest_variables(
    pool: &PgPool,
    character_quest_id: &str,
) -> sqlx::Result<Option<serde_json::Value>> {
    let row = sqlx::query!(
        r#"
        SELECT variables AS "variables!: serde_json::Value"
        FROM "CharacterQuest"
        WHERE id = $1
        "#,
        character_quest_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.variables))
}

/// Set one named key inside the per-character quest variables JSON
/// object (Wave 4.10). Other keys preserved. `Null` deletes the key.
pub async fn set_quest_variable(
    pool: &PgPool,
    character_quest_id: &str,
    name: &str,
    value: &serde_json::Value,
) -> sqlx::Result<u64> {
    let path: Vec<String> = vec![name.to_string()];
    let result = if value.is_null() {
        sqlx::query!(
            r#"
            UPDATE "CharacterQuest"
            SET variables = variables - $2
            WHERE id = $1
            "#,
            character_quest_id,
            name,
        )
        .execute(pool)
        .await?
    } else {
        sqlx::query!(
            r#"
            UPDATE "CharacterQuest"
            SET variables = jsonb_set(variables, $2::text[], $3::jsonb, true)
            WHERE id = $1
            "#,
            character_quest_id,
            path.as_slice(),
            value,
        )
        .execute(pool)
        .await?
    };
    Ok(result.rows_affected())
}

/// Find a CharacterQuest row by `(character_id, quest_zone, quest_id)`.
/// Returns the row id + status. Used by trigger dispatchers
/// (Wave 4.1) to detect whether the player is already on this quest
/// before re-prompting them.
pub async fn find_character_quest(
    pool: &PgPool,
    character_id: &str,
    zone_id: i32,
    quest_id: i32,
) -> sqlx::Result<Option<(String, String)>> {
    let row = sqlx::query!(
        r#"
        SELECT id, status::text AS "status!: String"
        FROM "CharacterQuest"
        WHERE character_id = $1
          AND quest_zone_id = $2
          AND quest_id = $3
        "#,
        character_id,
        zone_id,
        quest_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.id, r.status)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_outcome_variants_distinct() {
        // The command-layer match in `cmd_qaccept` relies on every
        // variant being structurally distinct so player-visible
        // refusal text doesn't accidentally collide.
        let level_low = AcceptOutcome::LevelTooLow(10);
        let level_high = AcceptOutcome::LevelTooHigh(50);
        let already_in = AcceptOutcome::AlreadyInProgress;
        let already_done = AcceptOutcome::AlreadyCompletedNonRepeatable;
        let prereq = AcceptOutcome::PrerequisiteIncomplete {
            zone: 30,
            id: 1,
        };
        let cooldown = AcceptOutcome::Cooldown {
            remaining_secs: 120,
        };
        let excl = AcceptOutcome::ExclusiveGroupConflict {
            group: "dragon".into(),
            held_zone: 30,
            held_id: 7,
        };
        let req = AcceptOutcome::RequirementNotMet {
            expression: "false".into(),
        };

        // Sanity-check inequality across the new (Wave 4) variants.
        assert_ne!(level_low, level_high);
        assert_ne!(already_in, already_done);
        assert_ne!(cooldown, excl);
        assert_ne!(req, excl);

        // Equality on identical payload.
        assert_eq!(
            AcceptOutcome::Cooldown { remaining_secs: 120 },
            cooldown
        );
        assert_eq!(
            AcceptOutcome::PrerequisiteIncomplete { zone: 30, id: 1 },
            prereq
        );
    }
}
