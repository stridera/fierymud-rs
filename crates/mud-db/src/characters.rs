use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};

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
    /// Lifetime seconds the character has been logged in across all
    /// sessions. Schema column defaults to 0; the runtime surfaces
    /// it via the `TimePlayed` component on score's "Played:" line.
    /// Incrementing on save is a follow-up — the column round-trips
    /// at zero for now until that lands.
    pub time_played: i32,
    /// Wall-clock timestamp of the previous session's login. Read
    /// once at spawn and stashed on a `PreviousLogin` component;
    /// the login flow's own `save_state` call later updates the
    /// column to `NOW()`, so this is the only spot to capture the
    /// "before-now" value for the score sheet's "Last login:"
    /// line. Null for brand-new characters who've never logged in.
    pub last_login: Option<chrono::NaiveDateTime>,
    /// Immortal-invisibility level. Mirrors `Characters.invis_level`.
    /// Zero means fully visible. Loaded into the `WizInvis(level)`
    /// component on login when nonzero; saved back from the same
    /// component on disconnect so the staff stays invis across
    /// sessions instead of popping into rooms on reconnect.
    pub invis_level: i32,
    /// Sanction level — when set, the player can't dispatch commands
    /// until an admin unfreezes them. Mirrors
    /// `Characters.freeze_level` (nullable in the schema; runtime
    /// reads None as "not frozen", any Some as frozen). Loaded into
    /// the `Frozen` marker; saved back as Some(1) when the marker
    /// is present.
    pub freeze_level: Option<i32>,
    /// Combat-flee threshold — flee attempt fires when HP drops below
    /// this percentage of max. 0 disables wimpy. Round-trips
    /// `WimpyThreshold` so a player's tuned threshold persists across
    /// disconnects (this is the kind of setting players set once and
    /// expect to stay set).
    pub wimpy_threshold: i32,
    /// Custom arrival / departure messages for staff teleports.
    /// `None` for either column → renderer falls back to the
    /// generic "$n appears" / "$n vanishes" lines. Round-trips
    /// the runtime's `Poofs` component.
    pub poof_in: Option<String>,
    pub poof_out: Option<String>,
}

/// Bundle of fields fed into `create` from the login-creation
/// flow. The schema's `Characters` table has many columns; most
/// have sensible defaults (level 1, hp 100, stamina 100, etc.).
/// Only the values the player actually picked are explicit here.
#[derive(Debug, Clone)]
pub struct NewCharacter<'a> {
    pub user_id: &'a str,
    pub name: &'a str,
    /// Schema enum text (`HUMAN` / `ELF` / …); cast to the
    /// `Race` type at INSERT time.
    pub race: &'a str,
    pub gender: &'a str,
    pub class_id: i32,
    pub strength: i32,
    pub intelligence: i32,
    pub wisdom: i32,
    pub dexterity: i32,
    pub constitution: i32,
    pub charisma: i32,
}

/// INSERT a fresh `Characters` row. Generates the id via
/// `gen_random_uuid()`; sets only the columns the player chose +
/// `password_hash = ''` (legacy column kept until the
/// account-side hash supplants it for live verification — auth
/// goes through `Users.password_hash` already, so this is
/// vestigial). Returns the new id so callers can store / log it.
///
/// Name uniqueness is enforced by the table's index — duplicate
/// names surface as `sqlx::Error::Database`. Callers should
/// have already checked uniqueness via `find_by_name`, but this
/// remains the defense in depth in case of a race with a
/// concurrent creation.
pub async fn create<'e, E: PgExecutor<'e>>(
    executor: E,
    new: &NewCharacter<'_>,
) -> sqlx::Result<String> {
    // sqlx::query (untyped) here so the runtime can pass the
    // race string straight into the SQL `::"Race"` cast — the
    // typed `query!` macro can't auto-map the schema's `Race`
    // enum to a Rust `&str` parameter, and adding a Rust-side
    // `Race` enum would mean keeping the 36-variant list in
    // sync with the schema. Cast does the validation: any
    // unknown variant errors at INSERT time.
    //
    // Generic over `PgExecutor` so callers can pair this with
    // `users::create` inside a transaction (the login-creation
    // flow uses `&mut *tx` for both INSERTs to keep the row
    // pair atomic).
    let row = sqlx::query_scalar::<_, String>(
        r#"
        INSERT INTO "Characters" (
            id, name, user_id, race, gender, class_id,
            strength, intelligence, wisdom, dexterity, constitution, charisma,
            password_hash, updated_at
        )
        VALUES (
            gen_random_uuid()::text, $1, $2, $3::"Race", $4, $5,
            $6, $7, $8, $9, $10, $11,
            '', NOW()
        )
        RETURNING id
        "#,
    )
    .bind(new.name)
    .bind(new.user_id)
    .bind(new.race)
    .bind(new.gender)
    .bind(new.class_id)
    .bind(new.strength)
    .bind(new.intelligence)
    .bind(new.wisdom)
    .bind(new.dexterity)
    .bind(new.constitution)
    .bind(new.charisma)
    .fetch_one(executor)
    .await?;
    Ok(row)
}

/// Persist mutable core attribute scores (str/dex/con/int/wis/cha)
/// back to `Characters`. Split from `save_state` so the latter
/// doesn't grow another 6 parameters; `train <stat>` is the first
/// runtime caller. Save-side only — the load path picks them up via
/// `list_for_user`.
/// Payload for `save_core_stats`. Carrying the six attribute scores
/// in a struct (rather than positional `i32` args) collapses the
/// call site to a single value and eliminates the swap risk —
/// passing `stats.dexterity, stats.strength, …` in the wrong order
/// would silently corrupt the persisted character.
#[derive(Debug, Clone, Copy)]
pub struct CoreStatsPayload {
    pub strength: i32,
    pub dexterity: i32,
    pub constitution: i32,
    pub intelligence: i32,
    pub wisdom: i32,
    pub charisma: i32,
}

pub async fn save_core_stats<'e, E: PgExecutor<'e>>(
    executor: E,
    character_id: &str,
    stats: &CoreStatsPayload,
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
        stats.strength,
        stats.dexterity,
        stats.constitution,
        stats.intelligence,
        stats.wisdom,
        stats.charisma,
        character_id,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Rename a character row by id. Returns the row count touched
/// (0 = id not found; 1 = success). Uniqueness on the `name`
/// column surfaces as a `sqlx::Error::Database` from the
/// underlying Postgres index — caller renders that as a "name
/// already taken" message.
pub async fn rename(
    pool: &PgPool,
    character_id: &str,
    new_name: &str,
) -> sqlx::Result<u64> {
    let res = sqlx::query!(
        r#"UPDATE "Characters" SET name = $1 WHERE id = $2"#,
        new_name,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Payload for `save_state`. Fifteen fields including four
/// `Option<i32>` room-pair coordinates (current zone/id +
/// recall zone/id) make the positional shape regression-prone:
/// a swap between any two `Option<i32>` arguments compiles
/// cleanly and silently persists the player into the wrong
/// room on next reconnect. Named-fields collapse the call site
/// to one struct literal that mirrors the SET-clause order.
#[derive(Debug, Clone)]
pub struct CharacterStatePayload<'a> {
    pub hit_points: i32,
    pub stamina: i32,
    pub current_room_zone_id: Option<i32>,
    pub current_room_id: Option<i32>,
    pub recall_room_zone_id: Option<i32>,
    pub recall_room_id: Option<i32>,
    pub player_flags: &'a [PlayerFlag],
    pub prompt: &'a str,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub wealth: i64,
    pub experience: i32,
    pub skill_points: i32,
    pub hunger: i32,
    pub thirst: i32,
    /// Wizinvis level — 0 means visible, otherwise the minimum
    /// observer level required to see this player. Round-trips
    /// the `WizInvis` component so staff stay invis on reconnect.
    pub invis_level: i32,
    /// Sanction lock — `Some(_)` keeps the player frozen across
    /// reconnects, `None` clears the lock. Pairs with the runtime's
    /// `Frozen` marker.
    pub freeze_level: Option<i32>,
    /// Wimpy threshold (HP%-of-max trigger for auto-flee). Pairs
    /// with the runtime's `WimpyThreshold` component.
    pub wimpy_threshold: i32,
    /// Custom arrival / departure messages for staff teleports.
    /// `None` writes NULL; the runtime falls back to the generic
    /// "$n appears" / "$n vanishes" lines at render time.
    pub poof_in: Option<&'a str>,
    pub poof_out: Option<&'a str>,
}

pub async fn save_state<'e, E: PgExecutor<'e>>(
    executor: E,
    character_id: &str,
    state: &CharacterStatePayload<'_>,
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
            invis_level = $16,
            freeze_level = $17,
            wimpy_threshold = $18,
            poof_in = $19,
            poof_out = $20
        WHERE id = $21
        "#,
        state.hit_points,
        state.stamina,
        state.current_room_zone_id,
        state.current_room_id,
        state.recall_room_zone_id,
        state.recall_room_id,
        state.player_flags as &[PlayerFlag],
        state.prompt,
        state.title,
        state.description,
        state.wealth,
        state.experience,
        state.skill_points,
        state.hunger,
        state.thirst,
        state.invis_level,
        state.freeze_level,
        state.wimpy_threshold,
        state.poof_in,
        state.poof_out,
        character_id,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Stamp `last_login = NOW()` exactly once per session at successful
/// login. Split out from `save_state` so generic autosave doesn't
/// keep resetting the column to "the most recent autosave" — the
/// prior behavior made the column mean "last save," which broke the
/// `clientinfo` "Last login" line and any downstream return-player
/// detection.
pub async fn update_last_login(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET last_login = NOW() WHERE id = $1"#,
        character_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Persist the bank balance separately from `save_state` (which
/// already takes too many args). Called from `save_player` when the
/// in-memory `BankWealth` differs from boot.
pub async fn save_bank_wealth<'e, E: PgExecutor<'e>>(
    executor: E,
    character_id: &str,
    bank_wealth: i64,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET bank_wealth = $1 WHERE id = $2"#,
        bank_wealth,
        character_id,
    )
    .execute(executor)
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

/// Persist the lifetime time-played counter. `save_player` calls
/// this with the cumulative seconds (existing column value plus
/// time elapsed since the previous save anchor) so the column
/// monotonically grows. Skip the call when the increment would be
/// zero — fresh saves immediately after another don't pay a write.
pub async fn save_time_played<'e, E: PgExecutor<'e>>(
    executor: E,
    character_id: &str,
    time_played: i32,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET time_played = $1 WHERE id = $2"#,
        time_played,
        character_id,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Save the drunkenness counter. Called from save-on-disconnect
/// alongside hunger / thirst so the value round-trips.
pub async fn save_drunkenness<'e, E: PgExecutor<'e>>(
    executor: E,
    character_id: &str,
    drunkenness: i32,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET drunkenness = $1 WHERE id = $2"#,
        drunkenness,
        character_id,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Read the JSON `script_vars` blob. Returns `Ok(None)` when the
/// column is NULL. Caller deserializes into the runtime
/// `ScriptVars(BTreeMap<String, String>)` shape.
pub async fn load_script_vars(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<Option<serde_json::Value>> {
    let row = sqlx::query!(
        r#"SELECT script_vars FROM "Characters" WHERE id = $1"#,
        character_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.script_vars))
}

/// Persist the `script_vars` blob. Pass `None` to clear the column
/// (player has no surviving vars); pass `Some(...)` to overwrite
/// with a fresh JSON object.
pub async fn save_script_vars<'e, E: PgExecutor<'e>>(
    executor: E,
    character_id: &str,
    blob: Option<&serde_json::Value>,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET script_vars = $1 WHERE id = $2"#,
        blob,
        character_id,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Read the JSON `trophy_data` blob. Returns `Ok(None)` when the
/// column is NULL. Caller deserializes into the runtime
/// `Trophy { entries: VecDeque<TrophyEntry> }` shape.
pub async fn load_trophy(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<Option<serde_json::Value>> {
    let row = sqlx::query!(
        r#"SELECT trophy_data FROM "Characters" WHERE id = $1"#,
        character_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.trophy_data))
}

/// Persist the `trophy_data` blob. Same shape as `save_script_vars` —
/// `None` clears the column, `Some(...)` overwrites.
pub async fn save_trophy<'e, E: PgExecutor<'e>>(
    executor: E,
    character_id: &str,
    blob: Option<&serde_json::Value>,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET trophy_data = $1 WHERE id = $2"#,
        blob,
        character_id,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Read the JSON `spell_cooldowns` blob — the in-flight slot cooldowns
/// for the legacy slot-pool casting model. Returns `Ok(None)` when the
/// column is NULL. Caller deserializes into the runtime
/// `SpellSlots { in_flight: Vec<SpellCooldown> }` shape.
pub async fn load_spell_cooldowns(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<Option<serde_json::Value>> {
    let row = sqlx::query!(
        r#"SELECT spell_cooldowns FROM "Characters" WHERE id = $1"#,
        character_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.spell_cooldowns))
}

/// Persist the `spell_cooldowns` blob. Pass `None` to clear the column
/// (no slots in cooldown — the common case at logout for a player
/// who's been resting).
pub async fn save_spell_cooldowns<'e, E: PgExecutor<'e>>(
    executor: E,
    character_id: &str,
    blob: Option<&serde_json::Value>,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET spell_cooldowns = $1 WHERE id = $2"#,
        blob,
        character_id,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Read the JSON `cooldowns` blob — per-ability cooldown ready-at
/// timestamps as unix seconds. Returns `Ok(None)` when NULL. Caller
/// converts the seconds to `Instant` and drops already-expired keys.
pub async fn load_cooldowns(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<Option<serde_json::Value>> {
    let row = sqlx::query!(
        r#"SELECT cooldowns FROM "Characters" WHERE id = $1"#,
        character_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.cooldowns))
}

/// Persist the `cooldowns` blob. Pass `None` to clear (caller has
/// no in-flight cooldowns — the common case for fresh logins).
pub async fn save_cooldowns<'e, E: PgExecutor<'e>>(
    executor: E,
    character_id: &str,
    blob: Option<&serde_json::Value>,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET cooldowns = $1 WHERE id = $2"#,
        blob,
        character_id,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Read the JSON `ignore_list` blob — array of lowercased names this
/// player has silenced. Returns `Ok(None)` when NULL.
pub async fn load_ignore_list(
    pool: &PgPool,
    character_id: &str,
) -> sqlx::Result<Option<serde_json::Value>> {
    let row = sqlx::query!(
        r#"SELECT ignore_list FROM "Characters" WHERE id = $1"#,
        character_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| r.ignore_list))
}

/// Persist the `ignore_list` blob. Pass `None` to clear (caller has
/// no entries — the typical-player case).
pub async fn save_ignore_list<'e, E: PgExecutor<'e>>(
    executor: E,
    character_id: &str,
    blob: Option<&serde_json::Value>,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE "Characters" SET ignore_list = $1 WHERE id = $2"#,
        blob,
        character_id,
    )
    .execute(executor)
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
            wealth, bank_wealth, gender, skill_points, hunger, thirst, time_played,
            last_login AS "last_login: chrono::NaiveDateTime",
            invis_level, freeze_level, wimpy_threshold,
            poof_in, poof_out
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
            thirst,
            time_played,
            last_login AS "last_login: chrono::NaiveDateTime",
            invis_level,
            freeze_level,
            wimpy_threshold,
            poof_in,
            poof_out
        FROM "Characters"
        WHERE user_id = $1
        ORDER BY level DESC, name
        "#,
        user_id
    )
    .fetch_all(pool)
    .await
}
