//! Lua trigger catalog rows + the three (mob/object/room) junction
//! tables that bind triggers to in-world entities.
//!
//! `Triggers.commands` is freeform text — typically the converted Lua
//! body produced by the legacy DG-Script-to-Lua pass. We don't parse
//! it here; the runtime stashes the trigger-key list per-entity as
//! `AttachedTriggers`, and a future dispatcher reads the body when it
//! actually needs to invoke the trigger.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[sqlx(type_name = "ScriptType", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScriptType {
    Mob,
    Object,
    World,
}

#[derive(sqlx::Type, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[sqlx(type_name = r#""TriggerFlag""#, rename_all = "SCREAMING_SNAKE_CASE")]
#[sqlx(no_pg_array)]
pub enum TriggerFlag {
    Global,
    Random,
    Command,
    Load,
    Cast,
    Leave,
    Time,
    Speech,
    Act,
    Death,
    Greet,
    GreetAll,
    Entry,
    Receive,
    Fight,
    HitPercent,
    Bribe,
    Memory,
    Door,
    SpeechTo,
    Look,
    Auto,
    Attack,
    Defend,
    Timer,
    Get,
    Drop,
    Give,
    Wear,
    Remove,
    Use,
    Consume,
    Reset,
    Preentry,
    Postentry,
}

// Same trick as PlayerFlag — the Postgres array type is mixed-case
// `_TriggerFlag`, so we override sqlx's lowercased default lookup.
impl sqlx::postgres::PgHasArrayType for TriggerFlag {
    fn array_type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name(r#""_TriggerFlag""#)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerRow {
    pub zone_id: i32,
    pub id: i32,
    pub name: String,
    pub attach_type: ScriptType,
    pub num_args: i32,
    pub arg_list: Option<Vec<String>>,
    pub commands: String,
    pub flags: Option<Vec<TriggerFlag>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MobTriggerLink {
    pub mob_zone_id: i32,
    pub mob_id: i32,
    pub trigger_zone_id: i32,
    pub trigger_id: i32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ObjectTriggerLink {
    pub object_zone_id: i32,
    pub object_id: i32,
    pub trigger_zone_id: i32,
    pub trigger_id: i32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RoomTriggerLink {
    pub room_zone_id: i32,
    pub room_id: i32,
    pub trigger_zone_id: i32,
    pub trigger_id: i32,
}

pub async fn list_triggers(pool: &PgPool) -> sqlx::Result<Vec<TriggerRow>> {
    sqlx::query_as!(
        TriggerRow,
        r#"
        SELECT
            zone_id,
            id,
            name,
            "attachType" AS "attach_type: ScriptType",
            num_args,
            arg_list AS "arg_list: Vec<String>",
            commands,
            flags AS "flags: Vec<TriggerFlag>"
        FROM "Triggers"
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}

pub async fn list_mob_triggers(pool: &PgPool) -> sqlx::Result<Vec<MobTriggerLink>> {
    sqlx::query_as!(
        MobTriggerLink,
        r#"
        SELECT mob_zone_id, mob_id, trigger_zone_id, trigger_id
        FROM "MobTriggers"
        "#
    )
    .fetch_all(pool)
    .await
}

pub async fn list_object_triggers(pool: &PgPool) -> sqlx::Result<Vec<ObjectTriggerLink>> {
    sqlx::query_as!(
        ObjectTriggerLink,
        r#"
        SELECT object_zone_id, object_id, trigger_zone_id, trigger_id
        FROM "ObjectTriggers"
        "#
    )
    .fetch_all(pool)
    .await
}

pub async fn list_room_triggers(pool: &PgPool) -> sqlx::Result<Vec<RoomTriggerLink>> {
    sqlx::query_as!(
        RoomTriggerLink,
        r#"
        SELECT room_zone_id, room_id, trigger_zone_id, trigger_id
        FROM "RoomTriggers"
        "#
    )
    .fetch_all(pool)
    .await
}
