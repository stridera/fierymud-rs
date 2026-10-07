//! Quest runtime behaviour that needs the live dev database (skipped
//! when it is unreachable): each test builds a throw-away quest and
//! character, drives the real command / bump paths, and cleans up.

#![allow(clippy::doc_markdown)]

use std::time::Duration;

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_db::sqlx::{self, PgPool};
use mud_world::{Account, Located, Named, Online, Player, WorldKey};

use super::test_support::{Rx, drain};
use super::{Connection, DbPool};

static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

struct Fx {
    pool: PgPool,
    char_id: String,
    zone: i32,
    quest: i32,
    room: (i32, i32),
}

async fn fixture() -> Option<Fx> {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
    let Ok(Ok(pool)) = tokio::time::timeout(Duration::from_secs(3), mud_db::connect(&url)).await
    else {
        eprintln!("skipping: dev database unavailable");
        return None;
    };
    let room: (i32, i32) =
        sqlx::query_as("SELECT zone_id, id FROM \"Room\" ORDER BY zone_id, id LIMIT 1")
            .fetch_optional(&pool)
            .await
            .ok()??;
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        + u128::from(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let char_id = format!("zz-qrs-c-{tag}");
    sqlx::query("INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW())")
        .bind(&char_id)
        .bind(format!("Zzs{}", tag % 1_000_000_000_000))
        .execute(&pool)
        .await
        .unwrap();
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let quest = 9_100_000 + (tag % 800_000) as i32;
    sqlx::query(
        "INSERT INTO \"Quest\" (zone_id, id, name, plain_name, repeatable, updated_at) \
         VALUES ($1, $2, 'zz rt test', 'zz rt test', true, NOW())",
    )
    .bind(room.0)
    .bind(quest)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO \"QuestPhase\" (quest_zone_id, quest_id, id, name, \"order\") \
         VALUES ($1, $2, 1, 'phase', 0)",
    )
    .bind(room.0)
    .bind(quest)
    .execute(&pool)
    .await
    .unwrap();
    Some(Fx {
        pool,
        char_id,
        zone: room.0,
        quest,
        room,
    })
}

impl Fx {
    /// VISIT_ROOM objective on the fixture room.
    async fn visit_objective(&self, required: i32) {
        sqlx::query(
            "INSERT INTO \"QuestObjective\" (quest_zone_id, quest_id, phase_id, id, \
             objective_type, player_description, required_count, \
             target_room_zone_id, target_room_id) \
             VALUES ($1, $2, 1, 1, 'VISIT_ROOM'::\"QuestObjectiveType\", 'visit', $3, $4, $5)",
        )
        .bind(self.zone)
        .bind(self.quest)
        .bind(required)
        .bind(self.room.0)
        .bind(self.room.1)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// A world holding the fixture player standing in a room entity.
    fn world(&self) -> (World, Entity, Entity, Rx) {
        self.world_as(UserRole::Player)
    }

    fn world_as(&self, role: UserRole) -> (World, Entity, Entity, Rx) {
        let mut world = World::new();
        world.insert_resource(DbPool(self.pool.clone()));
        let room = world
            .spawn(WorldKey {
                zone: self.room.0,
                id: self.room.1,
            })
            .id();
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        let player = world
            .spawn((
                Player,
                Online,
                Named {
                    name: "Quester".into(),
                },
                Account {
                    user_id: "u".into(),
                    character_id: self.char_id.clone(),
                    role,
                    account_role: role,
                    perms: Vec::new(),
                },
                Connection(tx),
                Located(room),
            ))
            .id();
        (world, player, room, rx)
    }

    /// Pay 50 XP + 7 gold on completion.
    async fn rewards(&self) {
        for (ty, amount) in [("EXPERIENCE", 50), ("GOLD", 7)] {
            sqlx::query(
                "INSERT INTO \"QuestReward\" (quest_zone_id, quest_id, phase_id, reward_type, amount) \
                 VALUES ($1, $2, 1, $3::\"QuestRewardType\", $4)",
            )
            .bind(self.zone)
            .bind(self.quest)
            .bind(ty)
            .bind(amount)
            .execute(&self.pool)
            .await
            .unwrap();
        }
    }

    async fn paid(&self) -> (i32, i64) {
        sqlx::query_as("SELECT experience, wealth FROM \"Characters\" WHERE id = $1")
            .bind(&self.char_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn accept(&self) {
        let outcome =
            mud_db::quests::accept_for_player(&self.pool, &self.char_id, 10, self.zone, self.quest)
                .await
                .unwrap();
        assert_eq!(outcome, mud_db::quests::AcceptOutcome::Accepted);
    }

    async fn status(&self) -> String {
        sqlx::query_scalar(
            "SELECT status::text FROM \"CharacterQuest\" \
             WHERE character_id = $1 AND quest_zone_id = $2 AND quest_id = $3",
        )
        .bind(&self.char_id)
        .bind(self.zone)
        .bind(self.quest)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// Poll until the quest reaches `want` (async bump tasks run in the
    /// background), or give up after a few seconds.
    async fn wait_for_status(&self, want: &str) -> String {
        let mut now = self.status().await;
        for _ in 0..60 {
            if now == want {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            now = self.status().await;
        }
        now
    }

    async fn end(self) {
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

/// A character who already walked through the room (so the exploration
/// set no longer reports it as new) still progresses a VISIT_ROOM
/// objective when they enter it while the quest is active.
#[tokio::test(flavor = "current_thread")]
async fn visit_room_counts_entries_after_an_earlier_visit() {
    let Some(fx) = fixture().await else { return };
    fx.visit_objective(2).await;
    let (mut world, player, room, mut rx) = fx.world();
    // The earlier visit, long before the quest: the room is already in
    // the player's exploration set, so `mark_room_visited` is a no-op.
    let mut visits = mud_world::ZoneVisits::default();
    visits
        .by_zone
        .entry(fx.room.0)
        .or_default()
        .insert(fx.room.1);
    world.entity_mut(player).insert(visits);
    fx.accept().await;

    for _ in 0..2 {
        super::mark_room_visited(&mut world, player, room);
        super::note_room_entry(&mut world, player, room);
        // Let the spawned DB task for this entry land before the next.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert_eq!(fx.wait_for_status("COMPLETED").await, "COMPLETED");
    let out = drain(&mut rx);
    assert!(out.contains("Quest objective: visit (1/2)"), "{out}");
    assert!(out.contains("*** Quest complete! ***"), "{out}");
    fx.end().await;
}

/// `qcomplete` is a staff shortcut to completion, so it pays the same
/// rewards a real completion does.
#[tokio::test(flavor = "current_thread")]
async fn qcomplete_pays_the_quest_rewards() {
    let Some(fx) = fixture().await else { return };
    fx.visit_objective(1).await;
    fx.rewards().await;
    let (mut world, player, _room, mut rx) = fx.world_as(UserRole::Builder);
    fx.accept().await;
    let before = fx.paid().await;

    let handled = super::try_dispatch_async(&mut world, player, &fx.pool, "qcomplete 1").await;
    assert!(handled);
    assert_eq!(fx.status().await, "COMPLETED");
    let after = fx.paid().await;
    assert_eq!(after.0 - before.0, 50, "experience paid");
    assert_eq!(after.1 - before.1, 7, "gold paid");
    let out = drain(&mut rx);
    assert!(out.contains("Force-completed quest"), "{out}");
    assert!(out.contains("+50 experience"), "{out}");
    assert!(out.contains("+7 gold"), "{out}");
    fx.end().await;
}

/// `qreset` wipes the character's record so the quest can be given
/// again; mortals cannot use it.
#[tokio::test(flavor = "current_thread")]
async fn qreset_clears_a_players_quest_state() {
    let Some(fx) = fixture().await else { return };
    fx.visit_objective(1).await;
    let (mut world, player, _room, mut rx) = fx.world_as(UserRole::Builder);
    fx.accept().await;
    let cq = mud_db::quests::find_character_quest(&fx.pool, &fx.char_id, fx.zone, fx.quest)
        .await
        .unwrap()
        .unwrap()
        .0;
    mud_db::quest_objectives::upsert_progress(&fx.pool, &cq, fx.zone, fx.quest, 1, 1, 1, true)
        .await
        .unwrap();
    let cmd = format!("qreset Quester {} {}", fx.zone, fx.quest);

    // A mortal is refused and nothing changes.
    let (mut mw, mortal, _r, mut mrx) = fx.world_as(UserRole::Player);
    super::try_dispatch_async(&mut mw, mortal, &fx.pool, &cmd).await;
    assert!(drain(&mut mrx).contains("You can't do that."));
    assert!(
        mud_db::quests::find_character_quest(&fx.pool, &fx.char_id, fx.zone, fx.quest)
            .await
            .unwrap()
            .is_some()
    );

    // Staff resets it (the target is the online "Quester").
    super::try_dispatch_async(&mut world, player, &fx.pool, &cmd).await;
    let out = drain(&mut rx);
    assert!(out.contains("Reset Quest"), "{out}");
    assert!(
        mud_db::quests::find_character_quest(&fx.pool, &fx.char_id, fx.zone, fx.quest)
            .await
            .unwrap()
            .is_none(),
        "record gone"
    );
    let progress: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM \"CharacterQuestObjective\" WHERE character_quest_id = $1",
    )
    .bind(&cq)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(progress, 0, "objective progress gone");

    // A clean slate: staff can load it again, and a second reset says so.
    assert!(
        mud_db::quests::admin_assign(&fx.pool, &fx.char_id, fx.zone, fx.quest)
            .await
            .unwrap()
            .is_some()
    );
    let _ = mud_db::quests::admin_reset(&fx.pool, &fx.char_id, fx.zone, fx.quest).await;
    super::try_dispatch_async(&mut world, player, &fx.pool, &cmd).await;
    assert!(drain(&mut rx).contains("no record"));
    fx.end().await;
}
