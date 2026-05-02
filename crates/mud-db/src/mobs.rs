use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::{MobBehavior, MobRole, ProtectedKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mob {
    pub zone_id: i32,
    pub id: i32,
    pub name: String,
    pub keywords: Vec<String>,
    pub room_description: String,
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
            protected_kind AS "protected_kind!: ProtectedKind"
        FROM "Mobs"
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
