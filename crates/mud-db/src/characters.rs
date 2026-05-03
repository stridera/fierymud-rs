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
    /// Practice points awarded on level-up; spent by `practice <ability>`
    /// to bump proficiency. Defaults to 0 in the schema.
    pub skill_points: i32,
    /// Hunger gauge — game-hours since last meal. 0 = sated. Drained
    /// stamina/hp once over threshold (legacy `CircleMUD`: ~48). Tick
    /// system not yet wired; column is loaded and persisted so the
    /// tick can come up later without touching login flow again.
    pub hunger: i32,
    /// Thirst gauge — game-hours since last drink. 0 = sated. Same
    /// tick contract as `hunger` but with a tighter threshold (~24).
    pub thirst: i32,
}

/// Persist mutable core attribute scores (str/dex/con/int/wis/cha)
/// back to `Characters`. Split from `save_state` so the latter
/// doesn't grow another 6 parameters; `train <stat>` is the first
/// runtime caller. Save-side only — the load path picks them up via
/// `list_for_user`.
#[allow(clippy::too_many_arguments)]
pub async fn save_core_stats(
    pool: &PgPool,
    character_id: &str,
    strength: i32,
    dexterity: i32,
    constitution: i32,
    intelligence: i32,
    wisdom: i32,
    charisma: i32,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        UPDATE "Characters"
        SET strength = $1,
            dexterity = $2,
            constitution = $3,
            intelligence = $4,
            wisdom = $5,
            charisma = $6
        WHERE id = $7
        "#,
        strength,
        dexterity,
        constitution,
        intelligence,
        wisdom,
        charisma,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(())
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
    skill_points: i32,
    hunger: i32,
    thirst: i32,
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
            skill_points = $13,
            hunger = $14,
            thirst = $15,
            last_login = NOW()
        WHERE id = $16
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
        skill_points,
        hunger,
        thirst,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Persist the bank balance separately from `save_state` (which
/// already takes too many args). Called from `save_player` when the
/// in-memory `BankWealth` differs from boot.
pub async fn save_bank_wealth(
    pool: &PgPool,
    character_id: &str,
    bank_wealth: i64,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET bank_wealth = $1 WHERE id = $2"#,
        bank_wealth,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Load the persisted drunkenness counter. Returns 0 for null /
/// missing rows. The runtime ticks this down over time (a future
/// pass) and bumps on alcoholic drinks.
pub async fn load_drunkenness(pool: &PgPool, character_id: &str) -> sqlx::Result<i32> {
    let row = sqlx::query!(
        r#"SELECT drunkenness FROM "Characters" WHERE id = $1"#,
        character_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map_or(0, |r| r.drunkenness))
}

/// Save the drunkenness counter. Called from save-on-disconnect
/// alongside hunger / thirst so the value round-trips.
pub async fn save_drunkenness(
    pool: &PgPool,
    character_id: &str,
    drunkenness: i32,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET drunkenness = $1 WHERE id = $2"#,
        drunkenness,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Read the staff-notes blob for a character (Builder+ visibility).
/// Returns Ok(None) when null. Single shared blob — appended to by
/// the runtime's `pnote add` path.
pub async fn load_staff_notes(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<Option<String>> {
    let row = sqlx::query!(
        r#"SELECT staff_notes FROM "Characters" WHERE id = $1"#,
        character_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.staff_notes))
}

/// Overwrite the staff-notes blob. Caller is responsible for the
/// concatenation (load → append → save) — the runtime treats the
/// column as a free-form append-only log with author prefixes.
pub async fn save_staff_notes(
    pool: &PgPool,
    character_id: &str,
    notes: &str,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET staff_notes = $1 WHERE id = $2"#,
        notes,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Read the raw `kill_tracking_data` JSON for a character. Returns
/// Ok(None) when the column is null. The JSON shape is owned by
/// the runtime today — `{ "total": <int>, ...future fields }`.
pub async fn load_kill_tracking(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<Option<serde_json::Value>> {
    let row = sqlx::query!(
        r#"SELECT kill_tracking_data FROM "Characters" WHERE id = $1"#,
        character_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.kill_tracking_data))
}

/// Overwrite the `kill_tracking_data` JSON. Caller is responsible
/// for the merge — the runtime reads the existing JSON, mutates,
/// and writes the full object back.
pub async fn save_kill_tracking(
    pool: &PgPool,
    character_id: &str,
    data: &serde_json::Value,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET kill_tracking_data = $1 WHERE id = $2"#,
        data,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn find_by_name(pool: &PgPool, name: &str) -> sqlx::Result<Option<CharacterRow>> {
    sqlx::query_as!(
        CharacterRow,
        r#"
        SELECT
            id, name, user_id, level, hit_points, hit_points_max, stamina, stamina_max,
            hit_roll, damage_roll, armor_class, alignment,
            permissions AS "permissions!: Vec<Permission>",
            player_flags AS "player_flags!: Vec<PlayerFlag>",
            prompt, current_room_zone_id, current_room_id,
            recall_room_zone_id, recall_room_id, class_id,
            race::text AS "race!: String",
            experience, title, description,
            strength, dexterity, constitution, intelligence, wisdom, charisma,
            wealth, bank_wealth, gender, skill_points, hunger, thirst
        FROM "Characters"
        WHERE LOWER(name) = LOWER($1)
        LIMIT 1
        "#,
        name
    )
    .fetch_optional(pool)
    .await
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
            gender,
            skill_points,
            hunger,
            thirst
        FROM "Characters"
        WHERE user_id = $1
        ORDER BY level DESC, name
        "#,
        user_id
    )
    .fetch_all(pool)
    .await
}
