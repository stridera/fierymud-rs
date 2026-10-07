#![allow(clippy::doc_markdown)]
//! Quest runtime DB behaviour against the live dev database.
//!
//! Each test builds its own throw-away quest (high random id) and
//! character, and removes them at the end. Tests skip (pass) when the
//! dev database is not reachable, like the other live-DB tests.

use mud_db::quest_objectives::{
    PhaseAdvance, list_kill_mob_progress, try_advance_phase, upsert_progress,
};
use mud_db::quests::{AcceptOutcome, accept_for_player};
use sqlx::PgPool;

struct Fx {
    pool: PgPool,
    char_id: String,
    zone: i32,
    quest: i32,
    mobs: [(i32, i32); 2],
}

async fn fixture() -> Option<Fx> {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
    let Ok(Ok(pool)) =
        tokio::time::timeout(std::time::Duration::from_secs(3), mud_db::connect(&url)).await
    else {
        eprintln!("skipping: dev database unavailable");
        return None;
    };
    let mobs: Vec<(i32, i32)> =
        sqlx::query_as("SELECT zone_id, id FROM \"Mobs\" ORDER BY zone_id, id LIMIT 2")
            .fetch_all(&pool)
            .await
            .ok()?;
    let zone: i32 = sqlx::query_scalar("SELECT id FROM \"Zones\" ORDER BY id LIMIT 1")
        .fetch_optional(&pool)
        .await
        .ok()??;
    if mobs.len() < 2 {
        eprintln!("skipping: need two Mobs rows");
        return None;
    }
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let char_id = format!("zz-qrt-c-{tag}");
    sqlx::query("INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW())")
        .bind(&char_id)
        .bind(format!("Zzq{}", tag % 1_000_000_000_000))
        .execute(&pool)
        .await
        .unwrap();
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let quest = 9_000_000 + (tag % 900_000) as i32;
    sqlx::query(
        "INSERT INTO \"Quest\" (zone_id, id, name, plain_name, repeatable, updated_at) \
         VALUES ($1, $2, 'zz runtime test', 'zz runtime test', true, NOW())",
    )
    .bind(zone)
    .bind(quest)
    .execute(&pool)
    .await
    .unwrap();
    Some(Fx {
        pool,
        char_id,
        zone,
        quest,
        mobs: [mobs[0], mobs[1]],
    })
}

impl Fx {
    async fn phase(&self, id: i32, order: i32) {
        sqlx::query(
            "INSERT INTO \"QuestPhase\" (quest_zone_id, quest_id, id, name, \"order\") \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(self.zone)
        .bind(self.quest)
        .bind(id)
        .bind(format!("phase {id}"))
        .bind(order)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// KILL_MOB objective on mob `mobs[mob]`, required count 1.
    async fn kill(&self, phase: i32, id: i32, mob: usize) {
        sqlx::query(
            "INSERT INTO \"QuestObjective\" (quest_zone_id, quest_id, phase_id, id, \
             objective_type, player_description, required_count, \
             target_mob_zone_id, target_mob_id) \
             VALUES ($1, $2, $3, $4, 'KILL_MOB'::\"QuestObjectiveType\", 'kill', 1, $5, $6)",
        )
        .bind(self.zone)
        .bind(self.quest)
        .bind(phase)
        .bind(id)
        .bind(self.mobs[mob].0)
        .bind(self.mobs[mob].1)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// COLLECT_ITEM objective on the first `Objects` row, required count 2.
    async fn collect(&self, phase: i32, id: i32) -> (i32, i32) {
        let obj: (i32, i32) =
            sqlx::query_as("SELECT zone_id, id FROM \"Objects\" ORDER BY zone_id, id LIMIT 1")
                .fetch_one(&self.pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO \"QuestObjective\" (quest_zone_id, quest_id, phase_id, id, \
             objective_type, player_description, required_count, \
             target_object_zone_id, target_object_id) \
             VALUES ($1, $2, $3, $4, 'COLLECT_ITEM'::\"QuestObjectiveType\", 'collect', 2, $5, $6)",
        )
        .bind(self.zone)
        .bind(self.quest)
        .bind(phase)
        .bind(id)
        .bind(obj.0)
        .bind(obj.1)
        .execute(&self.pool)
        .await
        .unwrap();
        obj
    }

    async fn accept(&self) -> AcceptOutcome {
        accept_for_player(&self.pool, &self.char_id, 10, self.zone, self.quest)
            .await
            .unwrap()
    }

    async fn cq_id(&self) -> String {
        sqlx::query_scalar(
            "SELECT id FROM \"CharacterQuest\" \
             WHERE character_id = $1 AND quest_zone_id = $2 AND quest_id = $3",
        )
        .bind(&self.char_id)
        .bind(self.zone)
        .bind(self.quest)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn kill_rows(&self, mob: usize) -> usize {
        list_kill_mob_progress(
            &self.pool,
            &self.char_id,
            self.mobs[mob].0,
            self.mobs[mob].1,
            true,
        )
        .await
        .unwrap()
        .len()
    }

    async fn complete(&self, cq: &str, phase: i32, obj: i32) {
        upsert_progress(&self.pool, cq, self.zone, self.quest, phase, obj, 1, true)
            .await
            .unwrap();
    }

    async fn end(self) {
        // Quest delete cascades to phases, objectives and CharacterQuest.
        sqlx::query("DELETE FROM \"Quest\" WHERE zone_id = $1 AND id = $2")
            .bind(self.zone)
            .bind(self.quest)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM \"Characters\" WHERE id = $1")
            .bind(&self.char_id)
            .execute(&self.pool)
            .await
            .unwrap();
    }
}

/// Objective progress only counts in the character's current phase.
#[tokio::test]
async fn progress_is_gated_to_the_current_phase() {
    let Some(fx) = fixture().await else { return };
    fx.phase(1, 0).await;
    fx.phase(2, 1).await;
    fx.kill(1, 1, 0).await;
    fx.kill(2, 1, 1).await;
    assert_eq!(fx.accept().await, AcceptOutcome::Accepted);
    let cq = fx.cq_id().await;

    // Phase 2's target is not eligible while phase 1 is current.
    assert_eq!(fx.kill_rows(1).await, 0, "later-phase objective is gated");
    assert_eq!(fx.kill_rows(0).await, 1, "current-phase objective counts");

    fx.complete(&cq, 1, 1).await;
    assert!(matches!(
        try_advance_phase(&fx.pool, &cq).await.unwrap(),
        PhaseAdvance::Advanced {
            new_phase_id: 2,
            ..
        }
    ));
    assert_eq!(fx.kill_rows(0).await, 0, "finished phase no longer counts");
    assert_eq!(fx.kill_rows(1).await, 1, "new current phase counts");

    fx.complete(&cq, 2, 1).await;
    assert_eq!(
        try_advance_phase(&fx.pool, &cq).await.unwrap(),
        PhaseAdvance::QuestComplete
    );
    fx.end().await;
}

/// A later-phase objective that is already complete (recorded before
/// gating existed) must not stall the quest: entering that phase
/// re-evaluates immediately and finishes the quest.
#[tokio::test]
async fn already_satisfied_later_phase_does_not_stall() {
    let Some(fx) = fixture().await else { return };
    fx.phase(1, 0).await;
    fx.phase(2, 1).await;
    fx.kill(1, 1, 0).await;
    fx.kill(2, 1, 1).await;
    assert_eq!(fx.accept().await, AcceptOutcome::Accepted);
    let cq = fx.cq_id().await;

    // Legacy data: the phase-2 objective was finished first.
    fx.complete(&cq, 2, 1).await;
    assert_eq!(
        try_advance_phase(&fx.pool, &cq).await.unwrap(),
        PhaseAdvance::Pending,
        "phase 1 is still open"
    );
    fx.complete(&cq, 1, 1).await;
    assert_eq!(
        try_advance_phase(&fx.pool, &cq).await.unwrap(),
        PhaseAdvance::QuestComplete,
        "finishing phase 1 walks straight through the satisfied phase 2"
    );
    fx.end().await;
}

/// A phase with no objectives can never be completed by play, so it is
/// not treated as automatically done.
#[tokio::test]
async fn empty_phase_does_not_auto_advance() {
    let Some(fx) = fixture().await else { return };
    fx.phase(1, 0).await;
    assert_eq!(fx.accept().await, AcceptOutcome::Accepted);
    let cq = fx.cq_id().await;
    assert_eq!(
        try_advance_phase(&fx.pool, &cq).await.unwrap(),
        PhaseAdvance::Pending
    );
    fx.end().await;
}

/// COLLECT objectives are only offered for the held-items recheck once
/// their phase is current.
#[tokio::test]
async fn collect_recheck_lists_only_the_current_phase() {
    let Some(fx) = fixture().await else { return };
    fx.phase(1, 0).await;
    fx.phase(2, 1).await;
    fx.kill(1, 1, 0).await;
    let target = fx.collect(2, 1).await;
    assert_eq!(fx.accept().await, AcceptOutcome::Accepted);
    let cq = fx.cq_id().await;
    let list = || async {
        mud_db::quest_objectives::list_current_collect_objectives(&fx.pool, &fx.char_id)
            .await
            .unwrap()
    };
    assert!(list().await.is_empty(), "phase 2 is not current yet");

    fx.complete(&cq, 1, 1).await;
    assert!(matches!(
        try_advance_phase(&fx.pool, &cq).await.unwrap(),
        PhaseAdvance::Advanced {
            new_phase_id: 2,
            ..
        }
    ));
    let rows = list().await;
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].object_zone_id, rows[0].object_id), target);
    assert_eq!((rows[0].required_count, rows[0].current_count), (2, 0));

    // Crediting the held items completes the objective and the quest.
    fx.complete(&cq, 2, 1).await;
    assert_eq!(
        try_advance_phase(&fx.pool, &cq).await.unwrap(),
        PhaseAdvance::QuestComplete
    );
    assert!(list().await.is_empty());
    fx.end().await;
}
