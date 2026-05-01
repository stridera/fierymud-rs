use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::{Permission, PlayerFlag};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterRow {
    pub id: String,
    pub name: String,
    pub user_id: Option<String>,
    pub level: i32,
    pub hit_points: i32,
    pub hit_points_max: i32,
    pub stamina: i32,
    pub stamina_max: i32,
    pub hit_roll: i32,
    pub damage_roll: i32,
    pub armor_class: i32,
    pub alignment: i32,
    pub permissions: Vec<Permission>,
    pub player_flags: Vec<PlayerFlag>,
    pub prompt: String,
    pub current_room_zone_id: Option<i32>,
    pub current_room_id: Option<i32>,
    pub recall_room_zone_id: Option<i32>,
    pub recall_room_id: Option<i32>,
    /// FK to `Class.id`. Optional in the schema; characters created
    /// before class selection has no class yet.
    pub class_id: Option<i32>,
    /// Schema enum (HUMAN / ELF / GNOME / ...) — kept as the raw text
    /// label since the runtime only displays it.
    pub race: String,
    pub experience: i32,
    /// Player-set "the Wanderer" / "Slayer of Kobolds" line shown after
    /// the character name on `who`. None when unset.
    pub title: Option<String>,
    /// Free-form prose shown to anyone using `examine` on this
    /// character. None when unset; rendered with XML-Lite color tags
    /// like room descriptions.
    pub description: Option<String>,
    /// Core attribute scores. Schema default is 13 so freshly-rolled
    /// characters always have something. Bonuses derive `(stat - 10) / 2`
    /// at use sites; the runtime stores the raw scores.
    pub strength: i32,
    pub dexterity: i32,
    pub constitution: i32,
    pub intelligence: i32,
    pub wisdom: i32,
    pub charisma: i32,
    /// On-hand wealth in copper units. Schema is a Postgres BIGINT;
    /// per-race `copperFactor` converts to display denominations at
    /// render time.
    pub wealth: i64,
    pub bank_wealth: i64,
    /// Schema column verbatim — typically "male" / "female" /
    /// "neutral" (the "Sex" enum but stored as text). Used by Lua
    /// triggers via `actor.gender` for gendered gating.
    pub gender: String,
}

// One UPDATE-per-column-set is the simplest call site for this many fields;
// a SaveState struct would just shuffle the names. Revisit if it grows.
#[allow(clippy::too_many_arguments)]
pub async fn save_state(
    pool: &PgPool,
    character_id: &str,
    hit_points: i32,
    stamina: i32,
    current_room_zone_id: Option<i32>,
    current_room_id: Option<i32>,
    recall_room_zone_id: Option<i32>,
    recall_room_id: Option<i32>,
    player_flags: &[PlayerFlag],
    prompt: &str,
    title: Option<&str>,
    description: Option<&str>,
    wealth: i64,
    experience: i32,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        UPDATE "Characters"
        SET hit_points = $1,
            stamina = $2,
            current_room_zone_id = $3,
            current_room_id = $4,
            recall_room_zone_id = $5,
            recall_room_id = $6,
            player_flags = $7,
            prompt = $8,
            title = $9,
            description = $10,
            wealth = $11,
            experience = $12,
            last_login = NOW()
        WHERE id = $13
        "#,
        hit_points,
        stamina,
        current_room_zone_id,
        current_room_id,
        recall_room_zone_id,
        recall_room_id,
        player_flags as &[PlayerFlag],
        prompt,
        title,
        description,
        wealth,
        experience,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_for_user(pool: &PgPool, user_id: &str) -> sqlx::Result<Vec<CharacterRow>> {
    sqlx::query_as!(
        CharacterRow,
        r#"
        SELECT
            id,
            name,
            user_id,
            level,
            hit_points,
            hit_points_max,
            stamina,
            stamina_max,
            hit_roll,
            damage_roll,
            armor_class,
            alignment,
            permissions AS "permissions!: Vec<Permission>",
            player_flags AS "player_flags!: Vec<PlayerFlag>",
            prompt,
            current_room_zone_id,
            current_room_id,
            recall_room_zone_id,
            recall_room_id,
            class_id,
            race::text AS "race!: String",
            experience,
            title,
            description,
            strength,
            dexterity,
            constitution,
            intelligence,
            wisdom,
            charisma,
            wealth,
            bank_wealth,
            gender
        FROM "Characters"
        WHERE user_id = $1
        ORDER BY level DESC, name
        "#,
        user_id
    )
    .fetch_all(pool)
    .await
}
