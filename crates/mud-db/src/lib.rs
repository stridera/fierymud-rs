use sqlx::postgres::{PgPool, PgPoolOptions};

pub mod abilities;
pub mod boards;
pub mod ability_damage_components;
pub mod ability_effects;
pub mod ability_messages;
pub mod ability_restrictions;
pub mod ability_saving_throw;
pub mod ability_targeting;
pub mod character_abilities;
pub mod character_items;
pub mod characters;
pub mod classes;
pub mod effects;
pub mod enums;
pub mod mail;
pub mod mob_reset_equipment;
pub mod mob_resets;
pub mod mobs;
pub mod object_abilities;
pub mod object_reset_contents;
pub mod object_resets;
pub mod objects;
pub mod quests;
pub mod room_exits;
pub mod rooms;
pub mod shops;
pub mod levels;
pub mod socials;
pub mod spell_slots;
pub mod triggers;
pub mod users;
pub mod zones;

pub use sqlx;

pub async fn connect(database_url: &str) -> sqlx::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(8)
        .connect(database_url)
        .await
}
