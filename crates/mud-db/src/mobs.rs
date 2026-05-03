use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::{MobBehavior, MobProfession, MobRole, ProtectedKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mob {
    pub zone_id: i32,
    pub id: i32,
    pub name: String,
    pub keywords: Vec<String>,
    pub room_description: String,
    /// Long-form description shown by `examine <mob>`. Empty
    /// string when the builder hasn't authored one — the runtime
    /// then falls back to the short `room_description` so examine
    /// always has *something* to render.
    pub examine_description: String,
    pub level: i32,
    pub alignment: i32,
    pub role: MobRole,
    pub hp_dice_num: i32,
    pub hp_dice_size: i32,
    pub hp_dice_bonus: i32,
    pub damage_dice_num: i32,
    pub damage_dice_size: i32,
    pub damage_dice_bonus: i32,
    pub hit_roll: i32,
    pub armor_class: i32,
    /// On-hand wealth in copper units, paid to the killer on death.
    /// Schema column is BIGINT; default 0.
    pub wealth: i64,
    /// FK to `Class.id`; None for classless mobs (most NPCs).
    /// Read by triggers via `actor.class` to gate class-specific
    /// dialogue (e.g. quest hint chains attached to guildmasters).
    pub class_id: Option<i32>,
    /// AI behavior flags from `Mobs.behaviors`. Empty for mobs the
    /// content authors haven't tagged.
    pub behaviors: Vec<MobBehavior>,
    /// "Kill the wrong target" marker. Drives the alignment
    /// penalty applied to the killer in `combat::handle_death`.
    pub protected_kind: ProtectedKind,
    /// Service roles (banker / shopkeeper / trainer / etc).
    /// Empty for ordinary mobs. Drives gating on `deposit` /
    /// shop interaction / etc.
    pub professions: Vec<MobProfession>,
    /// Schema's `Gender` enum, kept as raw text for direct
    /// comparison with player gender strings (`male` / `female`
    /// / `neutral` / `non_binary`). Lua trigger bodies pattern-
    /// match against this via `actor.gender`.
    pub gender: String,
    /// Schema's `Race` enum, kept as raw text. Default `HUMANOID`
    /// for unspecified mobs. Lua bodies use this for
    /// `actor.race == "elf"`-style gating.
    pub race: String,
}

pub async fn list_mobs(pool: &PgPool) -> sqlx::Result<Vec<Mob>> {
    sqlx::query_as!(
        Mob,
        r#"
        SELECT
            zone_id,
            id,
            name,
            keywords AS "keywords!: Vec<String>",
            room_description,
            examine_description,
            level,
            alignment,
            role AS "role: MobRole",
            hp_dice_num,
            hp_dice_size,
            hp_dice_bonus,
            damage_dice_num,
            damage_dice_size,
            damage_dice_bonus,
            hit_roll,
            armor_class,
            wealth,
            class_id,
            behaviors AS "behaviors!: Vec<MobBehavior>",
            protected_kind AS "protected_kind!: ProtectedKind",
            professions AS "professions!: Vec<MobProfession>",
            gender::text AS "gender!",
            race::text AS "race!"
        FROM "Mobs"
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
