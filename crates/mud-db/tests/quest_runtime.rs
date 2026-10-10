#![allow(clippy::doc_markdown)]
//! Quest runtime DB behaviour against the live dev database.
//!
//! Each test builds its own throw-away quest (high random id) and
//! character, and removes them at the end. Tests skip (pass) when the
//! dev database is not reachable, like the other live-DB tests.

use mud_db::quest_objectives::{
    PhaseAdvance, QuestRewardRow, grant_simple_rewards, list_kill_mob_progress, try_advance_phase,
    upsert_progress,
};
use mud_db::quests::{AcceptOutcome, accept_for_player};
use sqlx::PgPool;

/// Keeps ids unique between tests running in parallel.
/// Small pools: every test opens its own, and the suite is routinely
/// run many copies at once against one shared database server.
fn test_pool_settings() -> mud_db::PoolSettings {
    mud_db::PoolSettings {
        max_connections: 1,
        acquire_timeout: std::time::Duration::from_secs(60),
    }
}

static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

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
    let Ok(Ok(pool)) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        mud_db::connect_with(&url, test_pool_settings()),
    )
    .await
    else {
        eprintln!("skipping: dev database unavailable");
        return None;
    };
    // Sweep fixtures a failed run left behind once they are clearly stale
    // (fresh ones may belong to suites running in parallel).
    sqlx::query(
        "DELETE FROM \"Quest\" WHERE name = 'zz runtime test' \
         AND created_at < NOW() - interval '10 minutes'",
    )
    .execute(&pool)
    .await
    .ok()?;
    sqlx::query(
        "DELETE FROM \"Characters\" WHERE id LIKE 'zz-qrt-c-%' \
         AND updated_at < NOW() - interval '10 minutes'",
    )
    .execute(&pool)
    .await
    .ok()?;
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
    // Unique per process and per call (copies of the suite may run at
    // once against the same database).
    let pid = std::process::id();
    let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let char_id = format!("zz-qrt-c-{pid}-{seq}-{nanos}");
    sqlx::query("INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW())")
        .bind(&char_id)
        .bind(format!("Zzq{pid}x{seq}x{}", nanos % 1_000_000))
        .execute(&pool)
        .await
        .unwrap();
    // Claim a quest id (draw until the insert wins) rather than compute one.
    let mut quest = 0;
    for attempt in 0..200_u128 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let candidate = 9_000_000
            + ((nanos / 7 + u128::from(pid) * 31 + u128::from(seq) * 977 + attempt * 7919)
                % 900_000) as i32;
        let inserted = sqlx::query(
            "INSERT INTO \"Quest\" (zone_id, id, name, plain_name, repeatable, updated_at) \
             VALUES ($1, $2, 'zz runtime test', 'zz runtime test', true, NOW()) \
             ON CONFLICT (zone_id, id) DO NOTHING",
        )
        .bind(zone)
        .bind(candidate)
        .execute(&pool)
        .await
        .unwrap()
        .rows_affected();
        if inserted == 1 {
            quest = candidate;
            break;
        }
    }
    assert_ne!(quest, 0, "could not claim a quest id");
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

/// Re-accepting a finished (repeatable) quest starts from zero instead
/// of keeping the old run's finished objectives.
#[tokio::test]
async fn reaccept_after_completion_resets_objective_counts() {
    let Some(fx) = fixture().await else { return };
    fx.phase(1, 0).await;
    fx.kill(1, 1, 0).await;
    assert_eq!(fx.accept().await, AcceptOutcome::Accepted);
    let cq = fx.cq_id().await;
    fx.complete(&cq, 1, 1).await;
    assert_eq!(
        try_advance_phase(&fx.pool, &cq).await.unwrap(),
        PhaseAdvance::QuestComplete
    );
    assert_eq!(fx.kill_rows(0).await, 0, "finished objective is not listed");

    assert_eq!(fx.accept().await, AcceptOutcome::Accepted, "repeatable");
    assert_eq!(fx.cq_id().await, cq, "the row is revived in place");
    let rows = list_kill_mob_progress(&fx.pool, &fx.char_id, fx.mobs[0].0, fx.mobs[0].1, true)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "objective is open again");
    assert_eq!(rows[0].current_count, 0);
    fx.end().await;
}

/// The same reset applies after an abandon, which is how a
/// non-repeatable quest is retried.
#[tokio::test]
async fn reaccept_after_abandon_resets_partial_progress() {
    let Some(fx) = fixture().await else { return };
    fx.phase(1, 0).await;
    sqlx::query(
        "INSERT INTO \"QuestObjective\" (quest_zone_id, quest_id, phase_id, id, \
         objective_type, player_description, required_count, \
         target_mob_zone_id, target_mob_id) \
         VALUES ($1, $2, 1, 1, 'KILL_MOB'::\"QuestObjectiveType\", 'kill', 3, $3, $4)",
    )
    .bind(fx.zone)
    .bind(fx.quest)
    .bind(fx.mobs[0].0)
    .bind(fx.mobs[0].1)
    .execute(&fx.pool)
    .await
    .unwrap();
    assert_eq!(fx.accept().await, AcceptOutcome::Accepted);
    let cq = fx.cq_id().await;
    upsert_progress(&fx.pool, &cq, fx.zone, fx.quest, 1, 1, 2, false)
        .await
        .unwrap();
    assert_eq!(mud_db::quests::abandon(&fx.pool, &cq).await.unwrap(), 1);

    assert_eq!(fx.accept().await, AcceptOutcome::Accepted);
    let rows = list_kill_mob_progress(&fx.pool, &fx.char_id, fx.mobs[0].0, fx.mobs[0].1, true)
        .await
        .unwrap();
    assert_eq!(rows[0].current_count, 0, "partial progress was wiped");
    fx.end().await;
}

/// Quest variables written before a restart are listed for boot
/// hydration, and re-accepting wipes them.
#[tokio::test]
async fn variables_are_listed_for_hydration() {
    let Some(fx) = fixture().await else { return };
    fx.phase(1, 0).await;
    fx.kill(1, 1, 0).await;
    assert_eq!(fx.accept().await, AcceptOutcome::Accepted);
    let cq = fx.cq_id().await;
    let mine = |rows: Vec<mud_db::quests::QuestVariablesRow>| {
        rows.into_iter()
            .filter(|r| r.character_id == fx.char_id)
            .collect::<Vec<_>>()
    };
    assert!(
        mine(mud_db::quests::list_with_variables(&fx.pool).await.unwrap()).is_empty(),
        "an empty bag is not listed"
    );

    mud_db::quests::set_quest_variable(&fx.pool, &cq, "stage", &serde_json::json!("two"))
        .await
        .unwrap();
    let rows = mine(mud_db::quests::list_with_variables(&fx.pool).await.unwrap());
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].quest_zone_id, rows[0].quest_id),
        (fx.zone, fx.quest)
    );
    assert_eq!(rows[0].variables, serde_json::json!({"stage": "two"}));

    // Abandon + accept again: the database bag is reset.
    mud_db::quests::abandon(&fx.pool, &cq).await.unwrap();
    assert_eq!(fx.accept().await, AcceptOutcome::Accepted);
    assert!(mine(mud_db::quests::list_with_variables(&fx.pool).await.unwrap()).is_empty());
    fx.end().await;
}

/// DELIVER_ITEM matches on item AND recipient; USE_SKILL on the
/// ability. Both read the columns the quest API already stores.
#[tokio::test]
async fn deliver_item_and_use_skill_objectives_match_their_targets() {
    let Some(fx) = fixture().await else { return };
    let obj: (i32, i32) =
        sqlx::query_as("SELECT zone_id, id FROM \"Objects\" ORDER BY zone_id, id LIMIT 1")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    let ability: i32 = sqlx::query_scalar("SELECT id FROM \"Ability\" ORDER BY id LIMIT 1")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    fx.phase(1, 0).await;
    sqlx::query(
        "INSERT INTO \"QuestObjective\" (quest_zone_id, quest_id, phase_id, id, \
         objective_type, player_description, required_count, \
         target_object_zone_id, target_object_id, deliver_to_mob_zone_id, deliver_to_mob_id) \
         VALUES ($1, $2, 1, 1, 'DELIVER_ITEM'::\"QuestObjectiveType\", 'deliver', 1, \
         $3, $4, $5, $6)",
    )
    .bind(fx.zone)
    .bind(fx.quest)
    .bind(obj.0)
    .bind(obj.1)
    .bind(fx.mobs[0].0)
    .bind(fx.mobs[0].1)
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO \"QuestObjective\" (quest_zone_id, quest_id, phase_id, id, \
         objective_type, player_description, required_count, target_ability_id) \
         VALUES ($1, $2, 1, 2, 'USE_SKILL'::\"QuestObjectiveType\", 'use', 1, $3)",
    )
    .bind(fx.zone)
    .bind(fx.quest)
    .bind(ability)
    .execute(&fx.pool)
    .await
    .unwrap();
    assert_eq!(fx.accept().await, AcceptOutcome::Accepted);

    let deliver = |mob: usize| {
        let pool = fx.pool.clone();
        let cid = fx.char_id.clone();
        let (mz, mid) = fx.mobs[mob];
        async move {
            mud_db::quest_objectives::list_deliver_item_progress(
                &pool, &cid, obj.0, obj.1, mz, mid, true,
            )
            .await
            .unwrap()
        }
    };
    assert_eq!(deliver(0).await.len(), 1, "right item to the right mob");
    assert!(deliver(1).await.is_empty(), "wrong recipient");

    let used =
        mud_db::quest_objectives::list_use_skill_progress(&fx.pool, &fx.char_id, ability, true)
            .await
            .unwrap();
    assert_eq!(used.len(), 1);
    assert_eq!(used[0].objective_id, 2);
    fx.end().await;
}

/// Simultaneous bumps are atomic: no lost steps, and exactly one of
/// them observes the transition to complete.
#[tokio::test]
async fn concurrent_increments_count_every_step_and_complete_once() {
    let Some(fx) = fixture().await else { return };
    fx.phase(1, 0).await;
    sqlx::query(
        "INSERT INTO \"QuestObjective\" (quest_zone_id, quest_id, phase_id, id, \
         objective_type, player_description, required_count, \
         target_mob_zone_id, target_mob_id) \
         VALUES ($1, $2, 1, 1, 'KILL_MOB'::\"QuestObjectiveType\", 'kill', 8, $3, $4)",
    )
    .bind(fx.zone)
    .bind(fx.quest)
    .bind(fx.mobs[0].0)
    .bind(fx.mobs[0].1)
    .execute(&fx.pool)
    .await
    .unwrap();
    assert_eq!(fx.accept().await, AcceptOutcome::Accepted);
    let cq = fx.cq_id().await;

    let mut tasks = Vec::new();
    for _ in 0..12 {
        let (pool, cq, zone, quest) = (fx.pool.clone(), cq.clone(), fx.zone, fx.quest);
        tasks.push(tokio::spawn(async move {
            mud_db::quest_objectives::increment_progress(&pool, &cq, zone, quest, 1, 1, 8)
                .await
                .unwrap()
        }));
    }
    let mut results = Vec::new();
    for t in tasks {
        results.push(t.await.unwrap());
    }
    let applied: Vec<_> = results.iter().flatten().collect();
    assert_eq!(applied.len(), 8, "bumps past completion are no-ops");
    assert_eq!(applied.iter().filter(|(_, done)| *done).count(), 1);
    let mut counts: Vec<i32> = applied.iter().map(|(c, _)| *c).collect();
    counts.sort_unstable();
    assert_eq!(counts, (1..=8).collect::<Vec<_>>(), "no lost updates");
    fx.end().await;
}

/// Two racing completion checks: one quest completion, one
/// completion_count bump.
#[tokio::test]
async fn concurrent_phase_checks_complete_the_quest_once() {
    let Some(fx) = fixture().await else { return };
    fx.phase(1, 0).await;
    fx.kill(1, 1, 0).await;
    assert_eq!(fx.accept().await, AcceptOutcome::Accepted);
    let cq = fx.cq_id().await;
    fx.complete(&cq, 1, 1).await;

    let mut tasks = Vec::new();
    for _ in 0..6 {
        let (pool, cq) = (fx.pool.clone(), cq.clone());
        tasks.push(tokio::spawn(async move {
            try_advance_phase(&pool, &cq).await.unwrap()
        }));
    }
    let mut completions = 0;
    for t in tasks {
        if t.await.unwrap() == PhaseAdvance::QuestComplete {
            completions += 1;
        }
    }
    assert_eq!(completions, 1, "exactly one caller sees the completion");
    let count: i32 =
        sqlx::query_scalar("SELECT completion_count FROM \"CharacterQuest\" WHERE id = $1")
            .bind(&cq)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    fx.end().await;
}

/// A HOUSING reward creates one house (with a foyer) and is a no-op
/// the second time, so re-completing a quest never yields two houses.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn housing_reward_is_granted_once() {
    let Some(fx) = fixture().await else { return };
    // Entrance fallback for races with no start room on record.
    sqlx::query(
        "UPDATE \"Characters\" SET current_room_zone_id = 1, current_room_id = 1 WHERE id = $1",
    )
    .bind(&fx.char_id)
    .execute(&fx.pool)
    .await
    .unwrap();
    let housing = QuestRewardRow {
        id: 1,
        reward_type: "HOUSING".into(),
        amount: None,
        object_zone_id: None,
        object_id: None,
        ability_id: None,
        quantity: 1,
        choice_group: None,
        condition: None,
    };
    let first = grant_simple_rewards(&fx.pool, &fx.char_id, std::slice::from_ref(&housing))
        .await
        .unwrap();
    assert!(first.house_created);
    let second = grant_simple_rewards(&fx.pool, &fx.char_id, &[housing.clone(), housing])
        .await
        .unwrap();
    assert!(!second.house_created, "repeat grant is a no-op");

    let house = mud_db::housing::for_character(&fx.pool, &fx.char_id)
        .await
        .unwrap()
        .expect("house exists");
    let rooms = mud_db::housing::rooms_for_house(&fx.pool, house.id)
        .await
        .unwrap();
    assert_eq!(rooms.len(), 1);
    assert_eq!(rooms[0].local_index, 0, "foyer");
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM player_houses WHERE character_id = $1")
            .bind(&fx.char_id)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);

    // A placed item keeps its label and enchantment through the row.
    let (oz, oid): (i32, i32) =
        sqlx::query_as("SELECT zone_id, id FROM \"Objects\" ORDER BY zone_id, id LIMIT 1")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    let custom = mud_db::housing::HouseItemCustom {
        name: Some("a sword (Grim)".into()),
        examine: Some("It hums.".into()),
        keywords: Some(vec!["sword".into(), "grim".into()]),
        alter: Some(mud_db::character_items::ItemAlter {
            applies: vec![mud_db::character_items::ItemApply {
                target: "accuracy".into(),
                amount: 2,
            }],
            flags_added: vec![mud_db::enums::ObjectFlag::Magic],
            ..Default::default()
        }),
        charges: Some(0),
    };
    let placed = mud_db::housing::place_item(&fx.pool, rooms[0].id, oz, oid, &custom, None)
        .await
        .unwrap();
    let items = mud_db::housing::items_for_house(&fx.pool, house.id)
        .await
        .unwrap();
    let row = items.iter().find(|i| i.id == placed).expect("placed row");
    assert_eq!(row.custom(), custom);
    mud_db::housing::remove_item(&fx.pool, placed)
        .await
        .unwrap();

    // Placing an item that was carried takes its pack row away in the same
    // transaction, so it can never be in both a pack and a house.
    let pack_row: i32 = sqlx::query_scalar(
        "INSERT INTO \"CharacterItems\" (character_id, object_zone_id, object_id, updated_at) \
         VALUES ($1, $2, $3, NOW()) RETURNING id",
    )
    .bind(&fx.char_id)
    .bind(oz)
    .bind(oid)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    let placed = mud_db::housing::place_item(
        &fx.pool,
        rooms[0].id,
        oz,
        oid,
        &mud_db::housing::HouseItemCustom::default(),
        Some(pack_row),
    )
    .await
    .unwrap();
    let pack_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM \"CharacterItems\" WHERE id = $1")
            .bind(pack_row)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(pack_rows, 0, "pack row removed by the placement");
    let items = mud_db::housing::items_for_house(&fx.pool, house.id)
        .await
        .unwrap();
    assert_eq!(items.iter().filter(|i| i.id == placed).count(), 1);
    // Removing it (pickup) deletes exactly that row, once.
    assert_eq!(
        mud_db::housing::remove_item(&fx.pool, placed)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        mud_db::housing::remove_item(&fx.pool, placed)
            .await
            .unwrap(),
        0
    );
    fx.end().await;
}
