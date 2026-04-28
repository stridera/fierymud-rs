use sqlx::postgres::{PgPool, PgPoolOptions};

pub mod effects;
pub mod enums;
pub mod mobs;
pub mod objects;
pub mod room_exits;
pub mod rooms;
pub mod zones;

pub use sqlx;

pub async fn connect(database_url: &str) -> sqlx::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(8)
        .connect(database_url)
        .await
}
