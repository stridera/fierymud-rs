//! Shared fixtures for the unit tests of the player-facing parity
//! commands (`economy`, `magic_focus`, `subclass`, ...). Test-only.

use bevy_ecs::prelude::*;
use mud_db::abilities::AbilityKind;
use mud_db::enums::{MobProfession, ObjectType};
use mud_world::{AbilityDef, Located, Named, ObjectProto, Player};

use crate::commands::Connection;

pub(crate) type Rx = tokio::sync::mpsc::Receiver<Vec<u8>>;

/// Process-wide gate for every unit test that talks to the live dev
/// database. The tests share one Postgres (also hit by sibling copies of
/// the suite), and a burst of parallel tests, each with its own pool, used
/// to exhaust its connection slots (`PoolTimedOut` on the first insert).
/// A `tokio` mutex never poisons, so one panicking test cannot cascade.
static DB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Hold for the whole body of a live-DB test; serialises it against the
/// other live-DB tests in this binary.
pub(crate) async fn db_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    DB_TEST_LOCK.lock().await
}

/// Small pools with a generous acquire timeout: every live-DB test opens its
/// own pool and several copies of the suite may share one database server.
pub(crate) fn db_test_pool_settings() -> mud_db::PoolSettings {
    mud_db::PoolSettings {
        max_connections: 4,
        acquire_timeout: std::time::Duration::from_secs(60),
    }
}

/// Everything the player has been sent so far, lossily decoded.
pub(crate) fn drain(rx: &mut Rx) -> String {
    let mut out = String::new();
    while let Ok(bytes) = rx.try_recv() {
        out.push_str(&String::from_utf8_lossy(&bytes));
    }
    out
}

/// Player named "Tester" standing in `room`, with an attached
/// connection whose receiver is returned for output assertions.
pub(crate) fn player_in(world: &mut World, room: Entity) -> (Entity, Rx) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let player = world
        .spawn((
            Player,
            Named {
                name: "Tester".to_string(),
            },
            Located(room),
            Connection(tx),
        ))
        .id();
    (player, rx)
}

pub(crate) fn object_proto(zone: i32, id: i32, kind: ObjectType) -> ObjectProto {
    ObjectProto {
        zone_id: zone,
        id,
        r#type: kind,
        name: "a test object".to_string(),
        keywords: vec!["object".to_string()],
        room_description: String::new(),
        examine_description: None,
        weight: 0.0,
        weight_reduction: 0.0,
        recall_rooms: None,
        level: 1,
        wear_flags: vec![],
        weapon_dice_num: 0,
        weapon_dice_size: 0,
        weapon_dice_bonus: 0,
        weapon_damage_type: None,
        cost: 0,
        portal_destination_vnum: None,
        board_id: None,
        liquid: None,
        light_fuel: None,
        armor_pct: 0,
        restricted_alignments: vec![],
        restricted_class_ids: vec![],
        restricted_races: vec![],
        extras: vec![],
        resistances: vec![],
        granted_effects: vec![],
        flags: vec![],
        restrictions: vec![],
        timer_hours: 0,
        decompose_timer: 0,
        allowed_races: vec![],
        min_size: None,
        max_size: None,
        camp_kit_tier: None,
    }
}

pub(crate) fn ability_def(id: i32, name: &str, kind: AbilityKind) -> AbilityDef {
    AbilityDef {
        id,
        name: name.to_string(),
        plain_name: name.to_ascii_uppercase(),
        description: None,
        kind,
        violent: false,
        combat_ok: true,
        in_combat_only: false,
        cast_time_rounds: 1,
        cooldown_ms: 0,
        is_area: false,
        min_position_label: "STANDING".to_string(),
        min_posture_rank: 9,
        target_scope: "SINGLE".to_string(),
        is_magical: true,
        sphere: None,
        damage_type: None,
        memorization_time: 0,
    }
}

pub(crate) fn mob_proto(zone: i32, id: i32, profession: MobProfession) -> mud_world::MobProto {
    mud_world::MobProto {
        zone_id: zone,
        id,
        name: "a test mob".to_string(),
        keywords: vec!["mob".to_string()],
        room_description: String::new(),
        examine_description: String::new(),
        gender: "male".to_string(),
        race: "human".to_string(),
        level: 5,
        alignment: 0,
        role: mud_db::enums::MobRole::Normal,
        hp_dice_num: 1,
        hp_dice_size: 1,
        hp_dice_bonus: 5,
        damage_dice_num: 1,
        damage_dice_size: 1,
        damage_dice_bonus: 0,
        accuracy: 0,
        evasion: 0,
        attack_power: 0,
        spell_power: 0,
        penetration_flat: 0,
        penetration_percent: 0,
        armor_rating: 0,
        damage_reduction_percent: 0,
        soak: 0,
        hardness: 0,
        perception: 0,
        concealment: 0,
        resistances: serde_json::json!({}),
        ward_percent: 0,
        wealth: 0,
        class_id: None,
        behaviors: Vec::new(),
        protected_kind: mud_db::enums::ProtectedKind::Normal,
        professions: vec![profession],
        size: mud_db::enums::Size::Medium,
        life_force: mud_db::enums::LifeForce::Life,
        damage_type: mud_db::enums::DamageType::Hit,
        move_points: 0,
        default_position: mud_db::enums::Position::Standing,
        traits: Vec::new(),
        movement_mode: mud_db::enums::MovementMode::Normal,
        default_movement_mode: mud_db::enums::MovementMode::Normal,
        aggression_formula: None,
    }
}
