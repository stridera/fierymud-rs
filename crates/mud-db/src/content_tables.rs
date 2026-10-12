//! Small content tables the runtime used to hard-code (data over code):
//! status flag AI values and action blocks (`prevents_*`), spell chant syllables, `SystemMessage` text pools and
//! the `%d` prompt cooldown letters on `Ability`, and the reserved character
//! names. Builders edit the rows in
//! Muditor; the boot loader reads them into ECS resources. Each table is
//! created by `fierylib/data/sql/2026-10-09-content-tables.sql` (the
//! `StatusFlagValue.prevents_*` columns by `2026-10-12-status-flag-prevents.sql`), so the loader
//! treats a missing table / column as "no rows" rather than a boot failure.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusFlagValueRow {
    pub flag: String,
    pub ai_value: i32,
    /// An effect carrying this flag blocks movement.
    pub prevents_movement: bool,
    /// An effect carrying this flag blocks spellcasting.
    pub prevents_casting: bool,
    /// An effect carrying this flag blocks speaking.
    pub prevents_speaking: bool,
}

/// Every status flag's AI worth and which actions it blocks.
pub async fn list_status_flag_values(pool: &PgPool) -> sqlx::Result<Vec<StatusFlagValueRow>> {
    sqlx::query_as!(
        StatusFlagValueRow,
        r#"
        SELECT flag AS "flag!", ai_value AS "ai_value!",
               prevents_movement AS "prevents_movement!",
               prevents_casting AS "prevents_casting!",
               prevents_speaking AS "prevents_speaking!"
        FROM "StatusFlagValue"
        ORDER BY flag
        "#,
    )
    .fetch_all(pool)
    .await
}

/// [`list_status_flag_values`] for a database that predates
/// `fierylib/data/sql/2026-10-12-status-flag-prevents.sql` (the `prevents_*`
/// columns do not exist yet): AI values survive, no flag blocks anything. Only
/// the loader's missing-column fallback should call this.
pub async fn list_status_flag_values_without_prevents(
    pool: &PgPool,
) -> sqlx::Result<Vec<StatusFlagValueRow>> {
    sqlx::query_as!(
        StatusFlagValueRow,
        r#"
        SELECT flag AS "flag!", ai_value AS "ai_value!",
               false AS "prevents_movement!",
               false AS "prevents_casting!",
               false AS "prevents_speaking!"
        FROM "StatusFlagValue"
        ORDER BY flag
        "#,
    )
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpellSyllableRow {
    pub syllable: String,
    pub replacement: String,
}

/// Spell chant syllables in match order (`sort_order`, then `id`).
pub async fn list_spell_syllables(pool: &PgPool) -> sqlx::Result<Vec<SpellSyllableRow>> {
    sqlx::query_as!(
        SpellSyllableRow,
        r#"
        SELECT syllable AS "syllable!", replacement AS "replacement!"
        FROM "SpellSyllable"
        ORDER BY sort_order, id
        "#,
    )
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReservedNameRow {
    pub name: String,
}

/// Every word refused as a character name (`ReservedNames` table; created by
/// `fierylib/data/sql/2026-10-10-reserved-names.sql` or the Muditor schema).
pub async fn list_reserved_names(pool: &PgPool) -> sqlx::Result<Vec<ReservedNameRow>> {
    sqlx::query_as!(
        ReservedNameRow,
        r#"
        SELECT name AS "name!"
        FROM "ReservedNames"
        ORDER BY name
        "#,
    )
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMessageRow {
    pub key: String,
    pub messages: Vec<String>,
}

/// Every `SystemMessage` row (a null `messages` array reads as empty).
pub async fn list_system_messages(pool: &PgPool) -> sqlx::Result<Vec<SystemMessageRow>> {
    sqlx::query_as!(
        SystemMessageRow,
        r#"
        SELECT key AS "key!", COALESCE(messages, ARRAY[]::text[]) AS "messages!"
        FROM "SystemMessage"
        ORDER BY key
        "#,
    )
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptLetterRow {
    pub letter: String,
    pub ability_id: i32,
}

/// Abilities that drive a `%d<letter>` prompt cooldown bar.
pub async fn list_prompt_letters(pool: &PgPool) -> sqlx::Result<Vec<PromptLetterRow>> {
    sqlx::query_as!(
        PromptLetterRow,
        r#"
        SELECT prompt_letter AS "letter!", id AS "ability_id!"
        FROM "Ability"
        WHERE prompt_letter IS NOT NULL
        ORDER BY prompt_letter, id
        "#,
    )
    .fetch_all(pool)
    .await
}
