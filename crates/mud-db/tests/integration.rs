use mud_db::{
    connect,
    effects::list_effects,
    mobs::list_mobs,
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
