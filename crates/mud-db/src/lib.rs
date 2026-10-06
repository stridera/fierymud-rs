use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

pub mod abilities;
pub mod ability_components;
pub mod ability_damage_components;
pub mod ability_effects;
pub mod ability_messages;
pub mod ability_restrictions;
pub mod ability_saving_throw;
pub mod ability_targeting;
pub mod account_items;
pub mod achievements;
pub mod audit;
pub mod bans;
pub mod boards;
pub mod character_abilities;
pub mod character_aliases;
pub mod character_items;
pub mod characters;
pub mod clans;
pub mod classes;
pub mod consumable_effects;
pub mod dialogue;
pub mod discord_config;
pub mod discord_links;
pub mod effects;
pub mod entity_variables;
pub mod enums;
pub mod events;
pub mod game_config;
pub mod game_login_code;
pub mod google_links;
pub mod help;
pub mod housing;
pub mod levels;
pub mod liquids;
pub mod login_message;
pub mod mail;
pub mod mob_reset_equipment;
pub mod mob_resets;
pub mod mobs;
pub mod object_abilities;
pub mod object_effects;
pub mod object_extra_descriptions;
pub mod object_reset_contents;
pub mod object_resets;
pub mod object_resistance;
pub mod objects;
pub mod quest_objectives;
pub mod quests;
pub mod race_abilities;
pub mod races;
pub mod reports;
pub mod room_environmental_effects;
pub mod room_exits;
pub mod room_extra_descriptions;
pub mod rooms;
pub mod script_errors;
pub mod shops;
pub mod socials;
pub mod spell_slots;
pub mod system_text;
pub mod tell_messages;
pub mod triggers;
pub mod users;
pub mod zones;

pub use sqlx;

/// Connect the shared pool. Every session runs with `timezone = UTC`
/// (startup option), so `NOW()` written into `timestamp without time
/// zone` columns is naive UTC, matching the `Utc::now().naive_utc()`
/// values the Rust side binds and compares against.
pub async fn connect(database_url: &str) -> sqlx::Result<PgPool> {
    let opts = database_url
        .parse::<PgConnectOptions>()?
        .options([("timezone", "UTC")]);
    PgPoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await
}
