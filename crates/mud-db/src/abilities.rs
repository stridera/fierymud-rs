//! Catalog of every ability in the game — spells, chants, songs, and
//! skills all live in the `Ability` table. This module reads just the
//! fields the runtime needs for a first-pass implementation: name,
//! kind, basic gating (`violent`, `in_combat_only`, `min_position`),
//! description text. The richer surface (`AbilityComponent`,
//! `AbilityEffect`, `AbilityRestrictions`, etc.) is loaded on demand
//! when the runtime grows real casting.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbilityKind {
    Spell,
    Chant,
    Song,
    Skill,
}

impl AbilityKind {
    /// Parse the schema's free-text `abilityType` column into our enum.
    /// Schema today only stores SPELL / CHANT / SONG / SKILL; anything
    /// else falls back to Skill (most generic) so a malformed row doesn't
    /// break the loader.
    #[must_use]
    pub fn from_label(s: &str) -> Self {
        match s {
            "SPELL" => Self::Spell,
            "CHANT" => Self::Chant,
            "SONG" => Self::Song,
            _ => Self::Skill,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Spell => "spell",
            Self::Chant => "chant",
            Self::Song => "song",
            Self::Skill => "skill",
        }
    }
}

// Five gating bool flags is over clippy's struct_excessive_bools threshold,
// but they each map 1:1 to a schema column with distinct semantics. A
// bitflags type would just be indirection.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityRow {
    pub id: i32,
    /// Display name including any markup (sometimes color-tagged).
    pub name: String,
    /// Lowercase-able plain text used for command-line lookup.
    pub plain_name: String,
    pub description: Option<String>,
    pub ability_type: String,
    pub violent: bool,
    pub combat_ok: bool,
    pub in_combat_only: bool,
    /// Casting time in combat rounds (1 = instant in our pacing).
    pub cast_time_rounds: i32,
    pub cooldown_ms: i32,
    pub is_area: bool,
    /// Minimum posture required to invoke. Schema enum (DEAD .. STANDING)
    /// — runtime only models the upper four (SLEEPING / RESTING / SITTING
    /// / STANDING) so anything below SLEEPING is satisfied by every
    /// runtime posture.
    pub min_position: String,
    /// AOE / single-target dispatch hint. `Ability.target_scope`
    /// enum column — the post-locked-design values are `SELF` /
    /// `SINGLE` / `ROOM_ALLIES` / `ROOM_ENEMIES` / `ROOM_ALL` /
    /// `ROOM_ENVIRONMENT` (legacy `CHAIN` / `CONE` / `LINE` /
    /// `AREA` / `GROUP` also still exist in the enum but no row
    /// uses them post-migration). Surfaced to the runtime as a
    /// string so the dispatcher can match on any of the values
    /// without the runtime needing to know about the legacy ones.
    pub target_scope: String,
    /// Gates ward-vs-armor mitigation routing per `combat.md`.
    /// `true` engages magical mitigation (Ward) at combat
    /// pipeline step 5; `false` routes purely through Armor.
    /// Default convention: `SPELL` / `CHANT` / `SONG` are magical;
    /// `SKILL` is mundane unless flagged otherwise (sphere-of-X
    /// magical-flavor skills). The Ward stat split lands later;
    /// today the column loads through the catalog as
    /// groundwork.
    pub is_magical: bool,
    /// Spell sphere — `FIRE` / `WATER` / `HEALING` / `ENCHANTMENT`
    /// etc. (`SpellSphere` schema enum). Players see this on the
    /// spells listing as a dim parenthetical so they can scan by
    /// elemental affinity without running `identify` per spell.
    /// Lowercased here so display surfaces don't have to
    /// title-case at every call site.
    pub sphere: Option<String>,
    /// Primary damage type — `FIRE` / `COLD` / `HOLY` etc.
    /// (`ElementType` schema enum). Loaded for future damage-
    /// affinity / vulnerability routing; not yet surfaced on
    /// player-facing readouts.
    pub damage_type: Option<String>,
    /// Extra slot-cooldown seconds layered on top of the per-circle
    /// `CIRCLE_RECOVER_TIME` base. Lets the catalog tax expensive
    /// spells (`harm`, `meteorswarm`) more than the circle alone
    /// would suggest. 0 for the vast majority — the base table
    /// already does most of the work.
    pub memorization_time: i32,
}

pub async fn list_all(pool: &PgPool) -> sqlx::Result<Vec<AbilityRow>> {
    sqlx::query_as!(
        AbilityRow,
        r#"
        SELECT
            id,
            name,
            plain_name,
            description,
            "abilityType" AS ability_type,
            violent,
            combat_ok,
            in_combat_only,
            cast_time_rounds,
            cooldown_ms,
            is_area,
            "minPosition"::text AS "min_position!: String",
            target_scope::text AS "target_scope!: String",
            is_magical,
            LOWER(sphere::text) AS "sphere?: String",
            LOWER(damage_type::text) AS "damage_type?: String",
            memorization_time
        FROM "Ability"
        ORDER BY id
        "#
    )
    .fetch_all(pool)
    .await
}
