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
pub mod creation_recipes;
pub mod dialogue;
pub mod discord_config;
pub mod discord_links;
pub mod effect_auras;
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
pub mod mob_default_effects;
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
pub mod player_corpses;
pub mod quest_objectives;
pub mod quests;
pub mod race_abilities;
pub mod race_effects;
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

/// Default pool size. The server host has 4 cores and the game loop is a
/// single thread, so the pool is not CPU-bound — it only has to cover the
/// background save writers, login/auth lookups and fire-and-forget command
/// writes with headroom. 8 was starving under autosave bursts (issue #29:
/// 4-6 s waits for a connection).
pub const DEFAULT_MAX_CONNECTIONS: u32 = 16;
/// Default time a caller waits for a free connection before the acquire
/// errors out. Bounded so a starved pool fails loudly instead of freezing
/// the (single-threaded) game loop indefinitely.
pub const DEFAULT_ACQUIRE_TIMEOUT_SECS: u64 = 10;

/// Pool sizing, overridable from the environment (`DB_MAX_CONNECTIONS`,
/// `DB_ACQUIRE_TIMEOUT_SECS`). The pool must exist before `GameConfig` can
/// be read from the database, so these are env-only. Unset, unparsable or
/// zero values fall back to the defaults above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSettings {
    pub max_connections: u32,
    pub acquire_timeout: std::time::Duration,
}

impl Default for PoolSettings {
    fn default() -> Self {
        Self {
            max_connections: DEFAULT_MAX_CONNECTIONS,
            acquire_timeout: std::time::Duration::from_secs(DEFAULT_ACQUIRE_TIMEOUT_SECS),
        }
    }
}

impl PoolSettings {
    /// Read the overrides through `get` (the process environment in
    /// production; a map in tests).
    #[must_use]
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let positive = |key: &str| {
            get(key)
                .and_then(|v| v.trim().parse::<u64>().ok())
                .filter(|n| *n > 0)
        };
        let defaults = Self::default();
        Self {
            max_connections: positive("DB_MAX_CONNECTIONS")
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(defaults.max_connections),
            acquire_timeout: positive("DB_ACQUIRE_TIMEOUT_SECS")
                .map_or(defaults.acquire_timeout, std::time::Duration::from_secs),
        }
    }

    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }
}

/// Connect the shared pool with [`PoolSettings::from_env`]. Every session
/// runs with `timezone = UTC` (startup option), so `NOW()` written into
/// `timestamp without time zone` columns is naive UTC, matching the
/// `Utc::now().naive_utc()` values the Rust side binds and compares against.
pub async fn connect(database_url: &str) -> sqlx::Result<PgPool> {
    connect_with(database_url, PoolSettings::from_env()).await
}

pub async fn connect_with(database_url: &str, settings: PoolSettings) -> sqlx::Result<PgPool> {
    let opts = database_url
        .parse::<PgConnectOptions>()?
        .options([("timezone", "UTC")]);
    PgPoolOptions::new()
        .max_connections(settings.max_connections)
        .acquire_timeout(settings.acquire_timeout)
        .connect_with(opts)
        .await
}

#[cfg(test)]
mod pool_settings_tests {
    use super::*;

    fn lookup(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn defaults_when_unset_or_invalid() {
        assert_eq!(
            PoolSettings::from_lookup(lookup(&[])),
            PoolSettings::default()
        );
        let bad = PoolSettings::from_lookup(lookup(&[
            ("DB_MAX_CONNECTIONS", "lots"),
            ("DB_ACQUIRE_TIMEOUT_SECS", "0"),
        ]));
        assert_eq!(bad, PoolSettings::default());
        assert_eq!(PoolSettings::default().max_connections, 16);
    }

    #[test]
    fn env_overrides_apply() {
        let s = PoolSettings::from_lookup(lookup(&[
            ("DB_MAX_CONNECTIONS", "24"),
            ("DB_ACQUIRE_TIMEOUT_SECS", "3"),
        ]));
        assert_eq!(s.max_connections, 24);
        assert_eq!(s.acquire_timeout, std::time::Duration::from_secs(3));
    }
}
