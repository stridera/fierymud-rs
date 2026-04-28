use mud_db::{
    character_items::{list_for, save_for, NewCharacterItem},
    connect,
    effects::list_effects,
    mob_resets::list_all as list_mob_resets,
    mobs::list_mobs,
    object_resets::list_all as list_object_resets,
    objects::list_objects,
    room_exits::list_exits,
    rooms::list_rooms,
    zones::list_zones,
};
use sqlx::PgPool;

async fn pool() -> PgPool {
    let _ = dotenvy::from_path("../../.env");
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    connect(&url).await.expect("connect to fierydev")
}

#[tokio::test]
#[ignore = "requires live fierydev DB; run with: cargo test -p mud-db -- --ignored"]
async fn lists_zones() {
    let zones = list_zones(&pool().await).await.expect("list zones");
    assert!(!zones.is_empty());
    let void = zones.iter().find(|z| z.id == 0).expect("zone 0 (Void)");
    assert_eq!(void.name, "Void");
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_rooms() {
    let rooms = list_rooms(&pool().await).await.expect("list rooms");
    assert!(rooms.len() > 1000, "expected many rooms, got {}", rooms.len());
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_exits() {
    let exits = list_exits(&pool().await).await.expect("list exits");
    assert!(exits.len() > 1000, "expected many exits, got {}", exits.len());
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_mobs() {
    let mobs = list_mobs(&pool().await).await.expect("list mobs");
    assert!(!mobs.is_empty());
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_objects() {
    let objects = list_objects(&pool().await).await.expect("list objects");
    assert!(!objects.is_empty());
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_effects() {
    let effects = list_effects(&pool().await).await.expect("list effects");
    assert!(!effects.is_empty());
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_mob_resets() {
    let resets = list_mob_resets(&pool().await).await.expect("list mob resets");
    // Imported world has thousands of mob resets.
    assert!(resets.len() > 1000, "expected many mob resets, got {}", resets.len());
    // Probability is a fraction in [0, 1].
    for r in &resets {
        assert!(r.probability >= 0.0 && r.probability <= 1.0, "probability oob: {r:?}");
        assert!(r.max_instances >= 1, "max_instances < 1: {r:?}");
    }
}

#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn lists_object_resets() {
    let resets = list_object_resets(&pool().await).await.expect("list object resets");
    assert!(!resets.is_empty(), "expected some object resets");
    for r in &resets {
        assert!(r.probability >= 0.0 && r.probability <= 1.0);
        assert!(r.max_instances >= 1);
    }
}

/// Round-trip a small inventory through `CharacterItems`. Uses the seeded
/// `TestWarrior` account ('testplayer') so we don't need to spin up a fresh
/// character. Restores whatever was there before so re-running the test
/// doesn't permanently nuke real data.
///
/// We reference real (zone, id) keys from the Objects table so the FK
/// constraint passes. Picks the lowest two object IDs we can find.
#[tokio::test]
#[ignore = "requires live fierydev DB"]
async fn round_trips_character_items() {
    let pool = pool().await;

    // Find TestWarrior's character_id.
    let row = sqlx::query!(
        r#"SELECT id FROM "Characters" WHERE name = 'TestWarrior' LIMIT 1"#
    )
    .fetch_optional(&pool)
    .await
    .expect("query")
    .expect("seed user TestWarrior must exist");
    let cid = row.id;

    // Pick two real Object keys we can FK to.
    let keys: Vec<(i32, i32)> = sqlx::query!(
        r#"SELECT zone_id, id FROM "Objects" ORDER BY zone_id, id LIMIT 2"#
    )
    .fetch_all(&pool)
    .await
    .expect("query")
    .into_iter()
    .map(|r| (r.zone_id, r.id))
    .collect();
    assert_eq!(keys.len(), 2, "expected at least two Objects in the DB");

    // Snapshot whatever's already on TestWarrior so we restore at end.
    let before = list_for(&pool, &cid).await.expect("list before");

    // Save a known set: one carried, one worn (BODY).
    let payload = vec![
        NewCharacterItem {
            object_zone_id: keys[0].0,
            object_id: keys[0].1,
            equipped_location: None,
        },
        NewCharacterItem {
            object_zone_id: keys[1].0,
            object_id: keys[1].1,
            equipped_location: Some("BODY".to_string()),
        },
    ];
    save_for(&pool, &cid, &payload).await.expect("save");

    let after = list_for(&pool, &cid).await.expect("list after");
    assert_eq!(after.len(), 2, "two rows after save");
    let worn: Vec<_> = after.iter().filter(|r| r.equipped_location.as_deref() == Some("BODY")).collect();
    assert_eq!(worn.len(), 1, "one worn-on-body row");
    let carried: Vec<_> = after.iter().filter(|r| r.equipped_location.is_none()).collect();
    assert_eq!(carried.len(), 1, "one carried row");

    // Restore the original set so re-runs are idempotent.
    let restore: Vec<NewCharacterItem> = before
        .iter()
        .map(|r| NewCharacterItem {
            object_zone_id: r.object_zone_id,
            object_id: r.object_id,
            equipped_location: r.equipped_location.clone(),
        })
        .collect();
    save_for(&pool, &cid, &restore).await.expect("restore");
}
