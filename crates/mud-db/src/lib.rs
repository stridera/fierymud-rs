use sqlx::postgres::{PgPool, PgPoolOptions};

pub mod abilities;
pub mod achievements;
pub mod audit;
pub mod bans;
pub mod boards;
pub mod ability_components;
pub mod ability_damage_components;
pub mod ability_effects;
pub mod ability_messages;
pub mod ability_restrictions;
pub mod ability_saving_throw;
pub mod ability_targeting;
pub mod character_abilities;
pub mod character_aliases;
pub mod consumable_effects;
pub mod character_items;
pub mod housing;
pub mod characters;
pub mod clans;
pub mod classes;
pub mod effects;
pub mod enums;
pub mod game_config;
pub mod mail;
pub mod mob_reset_equipment;
pub mod mob_resets;
pub mod mobs;
pub mod object_abilities;
pub mod object_extra_descriptions;
pub mod object_reset_contents;
pub mod object_resets;
pub mod objects;
pub mod quest_objectives;
pub mod quests;
pub mod race_abilities;
pub mod races;
pub mod reports;
pub mod room_exits;
pub mod room_extra_descriptions;
pub mod room_environmental_effects;
pub mod rooms;
pub mod script_errors;
pub mod shops;
pub mod levels;
pub mod login_message;
pub mod socials;
pub mod spell_slots;
pub mod system_text;
pub mod tell_messages;
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
