use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct Effect {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    pub effect_type: String,
    pub tags: Vec<String>,
    pub presence_override: Option<String>,
    /// JSONB blob of the effect's default parameters (duration, amount,
    /// target field, etc.). The `AbilityEffect` row's `override_params`
    /// takes precedence; this is the secondary fallback before the
    /// runtime's hardcoded global default.
    pub default_params: serde_json::Value,
    /// When true, any character with this effect active can't run
    /// say/tell/gossip/shout/music/petition/insult/wiznet. The
    /// runtime gates these channels at dispatch time.
    pub prevents_speaking: bool,
    /// When true, blocks `cast`/`chant`/`perform` (and other paths
    /// that go through `invoke_ability` for SPELL kind).
    pub prevents_casting: bool,
    /// When true, blocks `cmd_move` (cardinal directions). Used for
    /// hold/web/paralyze flavors of immobilization.
    pub prevents_movement: bool,
    /// Lua source executed when an `EffectInstance` of this kind is
    /// first applied to a target. `self` binds to the target. Used
    /// for "you begin to drown" style on-apply flavor or one-shot
    /// state setup.
    pub on_apply: Option<String>,
    /// Lua source executed once per second while the effect is
    /// active. `self` binds to the target. Used for damage-over-time
    /// or atmospheric tick lines that don't justify a runtime hook.
    pub on_tick: Option<String>,
    /// Lua source executed when the effect is removed (expiry,
    /// dispel, cleanse, or target despawn). `self` binds to the
    /// target. Used for "you gasp for air" style fade messages.
    pub on_remove: Option<String>,
}

pub async fn list_effects(pool: &PgPool) -> sqlx::Result<Vec<Effect>> {
    sqlx::query_as!(
        Effect,
        r#"
        SELECT
            id,
            name,
            description,
            "effectType" AS effect_type,
            tags AS "tags!: Vec<String>",
            presence_override,
            default_params,
            prevents_speaking,
            prevents_casting,
            prevents_movement,
            on_apply,
            on_tick,
            on_remove
        FROM "Effect"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
