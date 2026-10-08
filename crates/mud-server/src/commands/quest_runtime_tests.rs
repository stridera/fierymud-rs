//! Quest runtime behaviour that needs the live dev database (skipped
//! when it is unreachable): each test builds a throw-away quest and
//! character, drives the real command / bump paths, and cleans up.

#![allow(clippy::doc_markdown)]

use std::time::Duration;

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_db::sqlx::{self, PgPool};
use mud_world::{Account, Item, Located, Named, Online, Player, WorldKey};

use super::test_support::{Rx, drain};
use super::{Connection, DbPool};

static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

struct Fx {
    /// Serialises live-DB tests; released when the fixture drops.
    _db_lock: tokio::sync::MutexGuard<'static, ()>,
    pool: PgPool,
    char_id: String,
    name: String,
    zone: i32,
    quest: i32,
    room: (i32, i32),
}

async fn fixture() -> Option<Fx> {
    let db_lock = super::test_support::db_test_lock().await;
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://strider@localhost/fierydev".into());
    let Ok(Ok(pool)) = tokio::time::timeout(
        Duration::from_secs(3),
        mud_db::connect_with(&url, super::test_support::db_test_pool_settings()),
    )
    .await
    else {
        eprintln!("skipping: dev database unavailable");
        return None;
    };
    // Fixtures a crashed or failed run left behind (a panic skips `end`)
    // are swept once they are clearly stale, so they cannot leak into
    // later runs; fresh ones may belong to tests running in parallel.
    sqlx::query(
        "DELETE FROM \"Quest\" WHERE name = 'zz rt test' \
         AND created_at < NOW() - interval '10 minutes'",
    )
    .execute(&pool)
    .await
    .ok()?;
    sqlx::query(
        "DELETE FROM \"Characters\" WHERE id LIKE 'zz-qrs-c-%' \
         AND updated_at < NOW() - interval '10 minutes'",
    )
    .execute(&pool)
    .await
    .ok()?;
    let room: (i32, i32) =
        sqlx::query_as("SELECT zone_id, id FROM \"Room\" ORDER BY zone_id, id LIMIT 1")
            .fetch_optional(&pool)
            .await
            .ok()??;
    // Unique per process AND per call, so copies of the suite running
    // against the same database never collide on character ids/names.
    let pid = std::process::id();
    let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let char_id = format!("zz-qrs-c-{pid}-{seq}-{nanos}");
    let name = format!("Zzs{pid}x{seq}x{}", nanos % 1_000_000);
    sqlx::query("INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW())")
        .bind(&char_id)
        .bind(&name)
        .execute(&pool)
        .await
        .unwrap();
    // Quest ids are claimed, not computed: draw until the insert wins, so
    // two fixtures can never share one.
    let mut quest = 0;
    for attempt in 0..200_u128 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let candidate = 9_100_000
            + ((nanos / 7 + u128::from(pid) * 31 + u128::from(seq) * 977 + attempt * 7919)
                % 800_000) as i32;
        let inserted = sqlx::query(
            "INSERT INTO \"Quest\" (zone_id, id, name, plain_name, repeatable, updated_at) \
             VALUES ($1, $2, 'zz rt test', 'zz rt test', true, NOW()) \
             ON CONFLICT (zone_id, id) DO NOTHING",
        )
        .bind(room.0)
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
        _db_lock: db_lock,
        pool,
        char_id,
        name,
        zone: room.0,
        quest,
        room,
    })
}

/// Everything the player is sent up to and including the first message
/// containing `needle` (waiting, bounded, for it to arrive).
async fn recv_through(rx: &mut Rx, needle: &str) -> String {
    let mut text = String::new();
    while !text.contains(needle) {
        let Ok(Some(bytes)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await else {
            panic!("timed out waiting for {needle:?}; got {text:?}");
        };
        text.push_str(&String::from_utf8_lossy(&bytes));
    }
    text
}

/// Drive the pack watch and the async-update drain until the player has
/// been sent a message containing `needle` (bounded), returning
/// everything received on the way. For flows that need the world
/// thread to cooperate; no fixed waits.
async fn pump_through(world: &mut World, rx: &mut Rx, needle: &str) -> String {
    let mut text = String::new();
    for _ in 0..200 {
        Fx::settle_once(world).await;
        while let Ok(bytes) = rx.try_recv() {
            text.push_str(&String::from_utf8_lossy(&bytes));
        }
        if text.contains(needle) {
            return text;
        }
    }
    panic!("timed out waiting for {needle:?}; got {text:?}");
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

    /// COLLECT_ITEM objective (phase 1, id 1) on the first `Objects` row.
    async fn collect_objective(&self, required: i32) -> (i32, i32) {
        let obj: (i32, i32) =
            sqlx::query_as("SELECT zone_id, id FROM \"Objects\" ORDER BY zone_id, id LIMIT 1")
                .fetch_one(&self.pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO \"QuestObjective\" (quest_zone_id, quest_id, phase_id, id, \
             objective_type, player_description, required_count, \
             target_object_zone_id, target_object_id) \
             VALUES ($1, $2, 1, 1, 'COLLECT_ITEM'::\"QuestObjectiveType\", 'collect', $3, $4, $5)",
        )
        .bind(self.zone)
        .bind(self.quest)
        .bind(required)
        .bind(obj.0)
        .bind(obj.1)
        .execute(&self.pool)
        .await
        .unwrap();
        obj
    }

    /// A second character (`<id>-alt`) who has also accepted the quest.
    async fn alt(&self) -> String {
        let id = format!("{}-alt", self.char_id);
        sqlx::query("INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW())")
            .bind(&id)
            .bind(format!("{}A", self.name))
            .execute(&self.pool)
            .await
            .unwrap();
        let outcome = mud_db::quests::accept_for_player(&self.pool, &id, 10, self.zone, self.quest)
            .await
            .unwrap();
        assert_eq!(outcome, mud_db::quests::AcceptOutcome::Accepted);
        id
    }

    /// Wire the async-update channel into `world` (what the tick loop
    /// owns in production).
    fn with_updates(world: &mut World) {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        world.insert_resource(super::PlayerUpdateTx(tx));
        world.insert_resource(super::PlayerUpdateInbox(std::sync::Mutex::new(rx)));
        world.insert_resource(crate::TickCount(0));
    }

    /// Run the pack watch and drain async updates a few times, giving
    /// the spawned DB tasks time to land in between.
    async fn settle(world: &mut World) {
        for _ in 0..10 {
            crate::quest_progress::collect_watch_tick(world);
            tokio::time::sleep(Duration::from_millis(60)).await;
            super::drain_player_updates(world);
        }
    }

    async fn settle_once(world: &mut World) {
        crate::quest_progress::collect_watch_tick(world);
        tokio::time::sleep(Duration::from_millis(60)).await;
        super::drain_player_updates(world);
    }

    fn give_item(world: &mut World, holder: Entity, key: (i32, i32)) -> Entity {
        world
            .spawn((
                Item,
                Named {
                    name: "a quest trinket".into(),
                },
                WorldKey {
                    zone: key.0,
                    id: key.1,
                },
                Located(holder),
            ))
            .id()
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
        .fetch_optional(&self.pool)
        .await
        .unwrap()
        .unwrap_or_else(|| "NONE".to_string())
    }

    /// Pump the world (pack watch + async-update drain) until the quest
    /// reaches `want`, bounded.
    async fn pump_to_status(&self, world: &mut World, want: &str) -> String {
        let mut now = self.status().await;
        for _ in 0..200 {
            if now == want {
                break;
            }
            Fx::settle_once(world).await;
            now = self.status().await;
        }
        now
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
        sqlx::query("DELETE FROM \"Characters\" WHERE id = $1 OR id = $2")
            .bind(&self.char_id)
            .bind(format!("{}-alt", self.char_id))
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
    }
    let out = recv_through(&mut rx, "*** Quest complete! ***").await;
    assert!(out.contains("Quest objective: visit (1/2)"), "{out}");
    assert_eq!(fx.status().await, "COMPLETED");
    fx.end().await;
}

/// `qcomplete` is a staff shortcut to completion, so it pays the same
/// rewards a real completion does.
#[tokio::test(flavor = "current_thread")]
async fn qcomplete_pays_the_quest_rewards() {
    let Some(fx) = fixture().await else { return };
    fx.visit_objective(1).await;
    fx.rewards().await;
    let (mut world, player, _room, mut rx) = fx.world_as(UserRole::Coder);
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

/// Racing completion checks (several bumps landing together) pay the
/// quest's rewards exactly once.
#[tokio::test(flavor = "current_thread")]
async fn racing_completions_pay_rewards_once() {
    let Some(fx) = fixture().await else { return };
    fx.visit_objective(1).await;
    fx.rewards().await;
    fx.accept().await;
    let cq = mud_db::quests::find_character_quest(&fx.pool, &fx.char_id, fx.zone, fx.quest)
        .await
        .unwrap()
        .unwrap()
        .0;
    mud_db::quest_objectives::upsert_progress(&fx.pool, &cq, fx.zone, fx.quest, 1, 1, 1, true)
        .await
        .unwrap();
    let before = fx.paid().await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let notify = crate::quest_progress::Notifier {
        character_id: fx.char_id.clone(),
        out: tx,
        update_tx: None,
    };
    let check = || crate::quest_progress::advance_quest(&fx.pool, &notify, &cq, fx.zone, fx.quest);
    tokio::join!(check(), check(), check(), check());

    let after = fx.paid().await;
    assert_eq!(after.0 - before.0, 50, "experience paid once");
    assert_eq!(after.1 - before.1, 7, "gold paid once");
    let mut text = String::new();
    while let Ok(b) = rx.try_recv() {
        text.push_str(&String::from_utf8_lossy(&b));
    }
    assert_eq!(text.matches("*** Quest complete! ***").count(), 1, "{text}");
    fx.end().await;
}

/// Triggers never re-grant a quest the character already has a record
/// of - in particular not one they abandoned.
#[tokio::test(flavor = "current_thread")]
async fn triggers_skip_quests_the_character_has_any_record_of() {
    let Some(fx) = fixture().await else { return };
    fx.visit_objective(1).await;
    sqlx::query("UPDATE \"Quest\" SET auto_accept = true WHERE zone_id = $1 AND id = $2")
        .bind(fx.zone)
        .bind(fx.quest)
        .execute(&fx.pool)
        .await
        .unwrap();
    let q = mud_db::quests::get_quest(&fx.pool, fx.zone, fx.quest)
        .await
        .unwrap()
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    fx.accept().await;
    let cq = mud_db::quests::find_character_quest(&fx.pool, &fx.char_id, fx.zone, fx.quest)
        .await
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(mud_db::quests::abandon(&fx.pool, &cq).await.unwrap(), 1);

    crate::quest_triggers::grant_or_offer(&fx.pool, &fx.char_id, &tx, 10, &q, None).await;
    assert_eq!(fx.status().await, "ABANDONED", "not re-granted");
    assert!(drain(&mut rx).is_empty(), "no message either");

    // A character with no record at all is still granted it.
    sqlx::query("DELETE FROM \"CharacterQuest\" WHERE id = $1")
        .bind(&cq)
        .execute(&fx.pool)
        .await
        .unwrap();
    crate::quest_triggers::grant_or_offer(&fx.pool, &fx.char_id, &tx, 10, &q, None).await;
    assert_eq!(fx.status().await, "IN_PROGRESS");
    assert!(drain(&mut rx).contains("New quest"));
    fx.end().await;
}

fn item_count(world: &mut World, key: (i32, i32)) -> usize {
    let mut q = world.query_filtered::<&WorldKey, With<Item>>();
    q.iter(world).filter(|wk| (wk.zone, wk.id) == key).count()
}

/// COLLECT progress is what is HELD: dropping and re-getting an item
/// cannot bank pickups, and completing the objective takes the items.
#[tokio::test(flavor = "current_thread")]
async fn collect_follows_the_pack_and_consumes_on_completion() {
    let Some(fx) = fixture().await else { return };
    let key = fx.collect_objective(2).await;
    let (mut world, player, room, mut rx) = fx.world();
    Fx::with_updates(&mut world);
    fx.accept().await;

    // get / drop loop with a single item never gets past 1/2.
    let item = Fx::give_item(&mut world, player, key);
    for _ in 0..3 {
        Fx::settle(&mut world).await;
        world.entity_mut(item).insert(Located(room)); // drop
        Fx::settle(&mut world).await;
        world.entity_mut(item).insert(Located(player)); // get
    }
    let out = pump_through(&mut world, &mut rx, "(1/2)").await;
    assert_eq!(fx.status().await, "IN_PROGRESS");
    assert!(out.contains("Quest objective: collect (1/2)"), "{out}");
    assert!(!out.contains("(2/2)"), "{out}");

    // A second item completes it, and both are handed over.
    Fx::give_item(&mut world, player, key);
    let out = pump_through(&mut world, &mut rx, "*** Quest complete! ***").await;
    assert_eq!(fx.status().await, "COMPLETED");
    assert_eq!(item_count(&mut world, key), 0, "items were consumed");
    assert!(out.contains("Quest objective complete: collect"), "{out}");
    fx.end().await;
}

/// The same items cannot complete the objective for two characters:
/// the first completion takes them.
#[tokio::test(flavor = "current_thread")]
async fn collect_items_cannot_be_passed_to_an_alt_to_complete_twice() {
    let Some(fx) = fixture().await else { return };
    let key = fx.collect_objective(2).await;
    let (mut world, player, room, _rx) = fx.world();
    Fx::with_updates(&mut world);
    fx.accept().await;
    let alt_id = fx.alt().await;
    let (tx2, _rx2) = tokio::sync::mpsc::channel(256);
    let alt = world
        .spawn((
            Player,
            Online,
            Named { name: "Alt".into() },
            Account {
                user_id: "u2".into(),
                character_id: alt_id.clone(),
                role: UserRole::Player,
                account_role: UserRole::Player,
                perms: Vec::new(),
            },
            Connection(tx2),
            Located(room),
        ))
        .id();

    let a = Fx::give_item(&mut world, player, key);
    let b = Fx::give_item(&mut world, player, key);
    assert_eq!(
        fx.pump_to_status(&mut world, "COMPLETED").await,
        "COMPLETED"
    );
    // Whatever the first character does next, nothing is left to pass on.
    assert!(world.get_entity(a).is_err() && world.get_entity(b).is_err());
    Fx::settle(&mut world).await;
    let alt_status: String = sqlx::query_scalar(
        "SELECT status::text FROM \"CharacterQuest\" WHERE character_id = $1 AND quest_id = $2",
    )
    .bind(&alt_id)
    .bind(fx.quest)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(alt_status, "IN_PROGRESS");
    assert_eq!(item_count(&mut world, key), 0);
    let _ = alt;
    fx.end().await;
}

/// Below coder rank `qcomplete` completes the quest but withholds the
/// rewards (a builder cannot mint XP / gold for themselves).
#[tokio::test(flavor = "current_thread")]
async fn qcomplete_withholds_rewards_below_coder() {
    let Some(fx) = fixture().await else { return };
    fx.visit_objective(1).await;
    fx.rewards().await;
    let (mut world, player, _room, mut rx) = fx.world_as(UserRole::HeadBuilder);
    fx.accept().await;
    let before = fx.paid().await;

    super::try_dispatch_async(&mut world, player, &fx.pool, "qcomplete 1").await;
    assert_eq!(fx.status().await, "COMPLETED");
    assert_eq!(fx.paid().await, before, "nothing paid");
    let out = drain(&mut rx);
    assert!(out.contains("Rewards withheld"), "{out}");
    assert!(!out.contains("+50 experience"), "{out}");
    fx.end().await;
}

/// Builders may reset their own record for testing, but only coder+
/// may reset another player's.
#[tokio::test(flavor = "current_thread")]
async fn qreset_of_other_players_needs_coder() {
    let Some(fx) = fixture().await else { return };
    fx.visit_objective(1).await;
    fx.accept().await;
    let alt_id = fx.alt().await;
    let alt_name: String = sqlx::query_scalar("SELECT name FROM \"Characters\" WHERE id = $1")
        .bind(&alt_id)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    let cmd = format!("qreset {alt_name} {} {}", fx.zone, fx.quest);
    let has_record = || async {
        mud_db::quests::find_character_quest(&fx.pool, &alt_id, fx.zone, fx.quest)
            .await
            .unwrap()
            .is_some()
    };

    let (mut world, builder, _room, mut rx) = fx.world_as(UserRole::Builder);
    super::try_dispatch_async(&mut world, builder, &fx.pool, &cmd).await;
    let out = drain(&mut rx);
    assert!(out.contains("needs coder rank"), "{out}");
    assert!(has_record().await, "builder could not reset another player");

    // ...but can reset themselves.
    let own = format!("qreset Quester {} {}", fx.zone, fx.quest);
    super::try_dispatch_async(&mut world, builder, &fx.pool, &own).await;
    assert!(drain(&mut rx).contains("Reset Quest"));

    let (mut world, coder, _room, mut rx) = fx.world_as(UserRole::Coder);
    super::try_dispatch_async(&mut world, coder, &fx.pool, &cmd).await;
    assert!(drain(&mut rx).contains("Reset Quest"));
    assert!(!has_record().await);
    fx.end().await;
}

/// Every qload / qgive / qcomplete / qreset use is audited, in the
/// runtime ring and in the `AuditLogs` table.
#[tokio::test(flavor = "current_thread")]
async fn quest_staff_commands_are_audited() {
    let Some(fx) = fixture().await else { return };
    let Ok(user) = sqlx::query_scalar::<_, String>("SELECT id FROM \"Users\" LIMIT 1")
        .fetch_one(&fx.pool)
        .await
    else {
        return;
    };
    fx.visit_objective(1).await;
    let (mut world, coder, room, _rx) = fx.world_as(UserRole::Coder);
    world.get_mut::<Account>(coder).unwrap().user_id = user.clone();
    let alt_id = format!("{}-alt", fx.char_id);
    let alt_name = format!("{}B", fx.name);
    sqlx::query("INSERT INTO \"Characters\" (id, name, updated_at) VALUES ($1, $2, NOW())")
        .bind(&alt_id)
        .bind(&alt_name)
        .execute(&fx.pool)
        .await
        .unwrap();
    let (tx2, _rx2) = tokio::sync::mpsc::channel(64);
    world.spawn((
        Player,
        Online,
        Named {
            name: alt_name.clone(),
        },
        Account {
            user_id: "u2".into(),
            character_id: alt_id,
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: Vec::new(),
        },
        Connection(tx2),
        Located(room),
    ));
    let (z, q) = (fx.zone, fx.quest);
    for cmd in [
        format!("qload {z} {q}"),
        "qcomplete 1".to_string(),
        format!("qreset Quester {z} {q}"),
        format!("qgive {alt_name} {z} {q}"),
    ] {
        assert!(super::try_dispatch_async(&mut world, coder, &fx.pool, &cmd).await);
    }
    let log = world.resource::<super::AdminAuditLog>();
    let verbs: Vec<&str> = log.entries.iter().map(|e| e.verb).collect();
    for verb in ["qload", "qcomplete", "qreset", "qgive"] {
        assert!(verbs.contains(&verb), "{verb} not audited: {verbs:?}");
    }
    // Persisted (fire-and-forget): wait for the rows.
    let needle = format!("%{z}:{q}%");
    let mut rows = 0;
    for _ in 0..40 {
        rows = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM \"AuditLogs\" WHERE user_id = $1 \
             AND action IN ('qload','qcomplete','qreset','qgive') \
             AND new_values->>'args' LIKE $2",
        )
        .bind(&user)
        .bind(&needle)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        if rows >= 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    sqlx::query("DELETE FROM \"AuditLogs\" WHERE user_id = $1 AND new_values->>'args' LIKE $2")
        .bind(&user)
        .bind(&needle)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(rows, 4, "one AuditLogs row per command");
    fx.end().await;
}

/// Trigger auto-accept runs the quest's availability requirement like
/// `qaccept`, and fails closed on a script error.
#[tokio::test(flavor = "current_thread")]
async fn trigger_auto_accept_honours_the_availability_requirement() {
    let Some(fx) = fixture().await else { return };
    fx.visit_objective(1).await;
    sqlx::query("UPDATE \"Quest\" SET auto_accept = true WHERE zone_id = $1 AND id = $2")
        .bind(fx.zone)
        .bind(fx.quest)
        .execute(&fx.pool)
        .await
        .unwrap();
    let (mut world, _player, _room, mut rx) = fx.world();
    Fx::with_updates(&mut world);
    world.insert_resource(mud_script::LuaHost::default());
    let tx = world.resource::<super::PlayerUpdateTx>().0.clone();
    let record = || async {
        mud_db::quests::find_character_quest(&fx.pool, &fx.char_id, fx.zone, fx.quest)
            .await
            .unwrap()
    };

    for denied in ["false", "((syntax error", "1", "error('boom')"] {
        sqlx::query(
            "UPDATE \"Quest\" SET availability_requirement = $3 WHERE zone_id = $1 AND id = $2",
        )
        .bind(fx.zone)
        .bind(fx.quest)
        .bind(denied)
        .execute(&fx.pool)
        .await
        .unwrap();
        let q = mud_db::quests::get_quest(&fx.pool, fx.zone, fx.quest)
            .await
            .unwrap()
            .unwrap();
        tx.send(super::PendingPlayerUpdate::TriggerCandidates {
            character_id: fx.char_id.clone(),
            quests: vec![q],
        })
        .await
        .unwrap();
        super::drain_player_updates(&mut world);
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(record().await.is_none(), "'{denied}' must deny the grant");
    }
    assert!(drain(&mut rx).is_empty(), "no offer text either");

    sqlx::query(
        "UPDATE \"Quest\" SET availability_requirement = 'true' WHERE zone_id = $1 AND id = $2",
    )
    .bind(fx.zone)
    .bind(fx.quest)
    .execute(&fx.pool)
    .await
    .unwrap();
    let q = mud_db::quests::get_quest(&fx.pool, fx.zone, fx.quest)
        .await
        .unwrap()
        .unwrap();
    tx.send(super::PendingPlayerUpdate::TriggerCandidates {
        character_id: fx.char_id.clone(),
        quests: vec![q],
    })
    .await
    .unwrap();
    super::drain_player_updates(&mut world);
    assert_eq!(fx.wait_for_status("IN_PROGRESS").await, "IN_PROGRESS");
    assert!(drain(&mut rx).contains("New quest"));
    fx.end().await;
}

/// Entering a trigger room offers the quest once per login session,
/// from the in-memory index (no query per step).
///
/// Isolated from whatever else is in the shared dev database: the index
/// holds only this test's quest (a full-table `refresh` would also pick
/// up unrelated ROOM-triggered quests), and the once-per-session rule is
/// checked through the dispatcher's return value rather than by waiting
/// for output to (not) arrive.
#[tokio::test(flavor = "current_thread")]
async fn room_trigger_offers_each_quest_once_per_session() {
    let Some(fx) = fixture().await else { return };
    sqlx::query(
        "UPDATE \"Quest\" SET trigger_type = 'ROOM'::\"QuestTriggerType\", \
         trigger_room_zone_id = $3, trigger_room_id = $4 WHERE zone_id = $1 AND id = $2",
    )
    .bind(fx.zone)
    .bind(fx.quest)
    .bind(fx.room.0)
    .bind(fx.room.1)
    .execute(&fx.pool)
    .await
    .unwrap();
    let ours = mud_db::quests::get_quest(&fx.pool, fx.zone, fx.quest)
        .await
        .unwrap()
        .unwrap();

    // `refresh` really loads ROOM-triggered quests from the database.
    let loaded = crate::quest_triggers::RoomQuestIndex::default();
    loaded.refresh(&fx.pool).await.unwrap();
    assert!(
        loaded
            .trigger_quests(fx.room)
            .iter()
            .any(|q| q.id == fx.quest && q.zone_id == fx.zone),
        "refresh loads the ROOM-triggered quest"
    );

    // The behaviour under test runs on an index holding only that quest.
    let (mut world, player, _room, mut rx) = fx.world();
    world.insert_resource(crate::quest_triggers::RoomQuestIndex::with(vec![ours], []));
    let enter = |w: &mut World, who| {
        crate::quest_triggers::dispatch_room_trigger(w, who, fx.room.0, fx.room.1)
    };
    assert_eq!(enter(&mut world, player), 1, "first entry offers it");
    for _ in 0..3 {
        assert_eq!(enter(&mut world, player), 0, "later entries do not");
    }
    let offer = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("offer text arrives")
        .expect("channel open");
    let offer = String::from_utf8_lossy(&offer).into_owned();
    assert!(
        offer.contains("Quest available")
            && offer.contains(&format!("({}, {})", fx.zone, fx.quest)),
        "{offer}"
    );

    // A fresh session (new entity) is offered it again.
    let (tx2, _rx2) = tokio::sync::mpsc::channel(64);
    let again = world
        .spawn((
            Player,
            Named {
                name: "Again".into(),
            },
            Account {
                user_id: "u".into(),
                character_id: fx.char_id.clone(),
                role: UserRole::Player,
                account_role: UserRole::Player,
                perms: Vec::new(),
            },
            Connection(tx2),
        ))
        .id();
    assert_eq!(enter(&mut world, again), 1);
    fx.end().await;
}

/// Rooms no VISIT_ROOM objective targets cost no quest query; targeted
/// rooms still progress.
#[tokio::test(flavor = "current_thread")]
async fn visit_room_bump_only_runs_for_targeted_rooms() {
    let Some(fx) = fixture().await else { return };
    fx.visit_objective(1).await;
    let (mut world, player, room, _rx) = fx.world();
    fx.accept().await;

    world.insert_resource(crate::quest_triggers::RoomQuestIndex::with(vec![], []));
    super::note_room_entry(&mut world, player, room);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fx.status().await, "IN_PROGRESS", "not a known target");

    world.insert_resource(crate::quest_triggers::RoomQuestIndex::with(
        vec![],
        [fx.room],
    ));
    super::note_room_entry(&mut world, player, room);
    assert_eq!(fx.wait_for_status("COMPLETED").await, "COMPLETED");
    fx.end().await;
}

/// Arrivals that are not steps - here a staff `goto` - count as
/// entering the room for VISIT_ROOM objectives; appearing in the world
/// does not.
#[tokio::test(flavor = "current_thread")]
async fn teleport_arrival_counts_for_visit_room() {
    let Some(fx) = fixture().await else { return };
    fx.visit_objective(1).await;
    let (mut world, player, target_room, _rx) = fx.world_as(UserRole::Builder);
    let start = world
        .spawn(WorldKey {
            zone: fx.room.0,
            id: 9_999_999,
        })
        .id();
    // The player begins in `start`; the objective's room is `target_room`.
    world.entity_mut(player).insert(Located(start));
    let mut index = mud_world::WorldKeyIndex::default();
    index.rooms.insert(fx.room, target_room);
    world.insert_resource(index);
    fx.accept().await;

    // First sighting only records the room, even if it were the target.
    super::room_entry_tick(&mut world);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fx.status().await, "IN_PROGRESS");

    super::dispatch(
        &mut world,
        player,
        &format!("goto {} {}", fx.room.0, fx.room.1),
    );
    assert_eq!(
        world.get::<Located>(player).map(|l| l.0),
        Some(target_room),
        "goto moved the player"
    );
    super::room_entry_tick(&mut world);
    assert_eq!(fx.wait_for_status("COMPLETED").await, "COMPLETED");
    fx.end().await;
}

/// A claim whose player vanished before the world thread drained it is
/// given back at once; one that is lost any other way expires and can
/// be taken again, so the quest is never wedged.
#[tokio::test(flavor = "current_thread")]
async fn orphaned_collect_claims_are_released_and_expire() {
    let Some(fx) = fixture().await else { return };
    let key = fx.collect_objective(2).await;
    let (mut world, _player, _room, _rx) = fx.world();
    Fx::with_updates(&mut world);
    fx.accept().await;
    let cq = mud_db::quests::find_character_quest(&fx.pool, &fx.char_id, fx.zone, fx.quest)
        .await
        .unwrap()
        .unwrap()
        .0;
    let claim = || async {
        mud_db::quest_objectives::claim_objective(&fx.pool, &cq, fx.zone, fx.quest, 1, 1, 2)
            .await
            .unwrap()
    };
    let row = || async {
        sqlx::query_as::<_, (i32, bool, bool)>(
            "SELECT current_count, completed, completed_at IS NOT NULL \
             FROM \"CharacterQuestObjective\" WHERE character_quest_id = $1",
        )
        .bind(&cq)
        .fetch_one(&fx.pool)
        .await
        .unwrap()
    };

    // The claim is made, then the player disconnects before it drains.
    assert!(claim().await);
    assert!(!claim().await, "a live claim cannot be taken twice");
    let tx = world.resource::<super::PlayerUpdateTx>().0.clone();
    tx.send(super::PendingPlayerUpdate::CollectClaimed {
        character_id: "nobody-online-with-this-id".into(),
        obj: crate::quest_progress::ObjectiveRef {
            character_quest_id: cq.clone(),
            quest_zone_id: fx.zone,
            quest_id: fx.quest,
            phase_id: 1,
            objective_id: 1,
            required_count: 2,
            show_progress: true,
            player_description: "collect".into(),
        },
        object: key,
    })
    .await
    .unwrap();
    super::drain_player_updates(&mut world);
    let mut released = false;
    for _ in 0..40 {
        if row().await == (0, false, false) {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(released, "unresolved player: claim released");
    assert_eq!(fx.status().await, "IN_PROGRESS");

    // A claim nobody ever consumes expires and is reclaimable.
    assert!(claim().await);
    assert!(!claim().await);
    sqlx::query(
        "UPDATE \"CharacterQuestObjective\" SET completed_at = NOW() - interval '31 seconds' \
         WHERE character_quest_id = $1",
    )
    .bind(&cq)
    .execute(&fx.pool)
    .await
    .unwrap();
    assert!(claim().await, "stale claim is taken over");
    fx.end().await;
}

/// Consuming a worn item takes it off properly: its stat bonuses go
/// with it, and the player is saved afterwards.
#[tokio::test(flavor = "current_thread")]
async fn consuming_a_worn_item_unequips_it_and_saves() {
    let Some(fx) = fixture().await else { return };
    let key = fx.collect_objective(1).await;
    let (mut world, player, _room, _rx) = fx.world();
    Fx::with_updates(&mut world);
    let fire = mud_db::enums::ElementType::Fire;
    world
        .entity_mut(player)
        .insert(mud_world::Resistances([(fire, 10)].into_iter().collect()));
    world
        .entity_mut(player)
        .insert(mud_world::Health { hp: 7, max: 10 });
    fx.accept().await;
    let item = Fx::give_item(&mut world, player, key);
    world.entity_mut(item).insert((
        mud_world::EquippedSlot(mud_world::Slot::Head),
        crate::equip_apply::GrantedDeltas {
            deltas: vec![],
            effects: vec![],
            resistances: vec![(fire, 10)],
        },
    ));

    assert_eq!(
        fx.pump_to_status(&mut world, "COMPLETED").await,
        "COMPLETED"
    );
    assert!(world.get_entity(item).is_err(), "worn item was consumed");
    assert!(
        world
            .get::<mud_world::Resistances>(player)
            .is_some_and(|r| r.0.is_empty()),
        "its resistance bonus was reversed"
    );
    // The removal is persisted promptly: the save lands in the database
    // (hit points 7 are only in the ECS) - or at least is queued for
    // the next tick when a write was already in flight.
    let mut saved = false;
    for _ in 0..40 {
        Fx::settle_once(&mut world).await;
        let hp: i32 = sqlx::query_scalar("SELECT hit_points FROM \"Characters\" WHERE id = $1")
            .bind(&fx.char_id)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
        if hp == 7 || world.get::<mud_world::PendingSave>(player).is_some() {
            saved = true;
            break;
        }
    }
    assert!(saved, "player save requested after the turn-in");
    fx.end().await;
}

/// Only players with active COLLECT objectives are watched.
#[tokio::test(flavor = "current_thread")]
async fn pack_watch_only_covers_players_with_collect_objectives() {
    let Some(fx) = fixture().await else { return };
    let key = fx.collect_objective(3).await;
    let (mut world, player, room, _rx) = fx.world();
    Fx::with_updates(&mut world);
    // A second player with no quest at all.
    let (tx2, _rx2) = tokio::sync::mpsc::channel(64);
    let idle = world
        .spawn((
            Player,
            Online,
            Named {
                name: "Idle".into(),
            },
            Account {
                user_id: "u2".into(),
                character_id: format!("{}-alt", fx.char_id),
                role: UserRole::Player,
                account_role: UserRole::Player,
                perms: Vec::new(),
            },
            Connection(tx2),
            Located(room),
        ))
        .id();
    fx.accept().await;
    let watched = |w: &World, e| {
        w.get::<crate::quest_progress::CollectWatch>(e)
            .map(|c| c.targets.clone())
    };
    for _ in 0..200 {
        Fx::settle_once(&mut world).await;
        if watched(&world, player).is_some_and(|t| !t.is_empty()) && watched(&world, idle).is_some()
        {
            break;
        }
    }
    assert_eq!(watched(&world, player), Some([key].into_iter().collect()));
    assert_eq!(
        watched(&world, idle),
        Some(std::collections::HashSet::new())
    );
    fx.end().await;
}
