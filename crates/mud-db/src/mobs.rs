use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::{
    DamageType, LifeForce, MobBehavior, MobProfession, MobRole, MobTrait, MovementMode, Position,
    ProtectedKind, Size,
};

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
    // Combat redesign axes — direct mirror of `Mobs` schema columns.
    // See `docs/design/combat.md` for the offense/defense split.
    pub accuracy: i32,
    pub evasion: i32,
    pub attack_power: i32,
    pub spell_power: i32,
    pub penetration_flat: i32,
    pub penetration_percent: i32,
    pub armor_rating: i32,
    pub damage_reduction_percent: i32,
    pub soak: i32,
    pub hardness: i32,
    pub perception: i32,
    pub concealment: i32,
    pub resistances: serde_json::Value,
    /// Magical mitigation percentage from `Mobs.ward_percent`. Engaged
    /// at combat pipeline step 5 when the damage source is magical.
    /// Zero on most mobs; raised on boss content.
    pub ward_percent: i32,
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
    /// Body / form size class. Drives bash/drag/mount size-disparity
    /// gates and the player-facing examine summary ("It is a HUGE
    /// creature."). `MEDIUM` is the schema default.
    pub size: Size,
    /// Vitality category — gates holy/unholy interactions, undead
    /// detection, and certain elemental immunities. `LIFE` is the
    /// default for organic mobs; UNDEAD/DEMONIC/CELESTIAL drive the
    /// faction-style ability filters.
    pub life_force: LifeForce,
    /// Natural attack flavor for the mob's unarmed swing (claws,
    /// bites, gores, etc). Drives the combat-narration verb in
    /// `attack_message`. `HIT` is the generic fallback.
    pub damage_type: DamageType,
    /// Movement-point pool size (legacy "move" column — renamed to
    /// avoid the Rust keyword collision). Mob's stamina equivalent
    /// for long wanders; consumed by `wander_tick`, restored by
    /// `regen_tick`. Zero on mobs that don't have a pool.
    pub move_points: i32,
    /// Initial posture on spawn. STANDING for the vast majority of
    /// mobs; SLEEPING for dragons in hoards, SITTING for tavern
    /// patrons, etc. `respawn::spawn_mob` derives the runtime
    /// `Posture` component from this.
    pub default_position: Position,
    /// Identity-flag list: what the mob IS (illusion / animated /
    /// mount / aquatic / summoned / pet / ...). Per-spawn instances
    /// receive a `MobTraits` component carrying the same list.
    pub traits: Vec<MobTrait>,
    /// Live movement mode at spawn — usually equals
    /// `default_movement_mode`. Loaded as a snapshot in case content
    /// authors want to author a non-default starting mode (e.g.
    /// flying drake patrolling above the cliffs).
    pub movement_mode: MovementMode,
    /// Reset / re-spawn movement mode. Re-applied each time the mob
    /// respawns from this proto. Tracks "this is how this creature
    /// fundamentally moves" — flying drakes always come back flying.
    pub default_movement_mode: MovementMode,
    /// Lua expression evaluated against entering players to decide
    /// whether this mob attacks on sight. None / empty → never
    /// aggressive. ~28% of mobs author a formula like
    /// `target.alignment >= ALIGN.GOOD` (alignment-keyed) or
    /// literal `true` (universally aggressive). The runtime
    /// consumer (aggression tick) isn't wired yet; this loads
    /// the column so `inspect mob` can surface it and so a
    /// future tick has the data ready.
    pub aggression_formula: Option<String>,
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
            accuracy,
            evasion,
            attack_power,
            spell_power,
            penetration_flat,
            penetration_percent,
            armor_rating,
            damage_reduction_percent,
            soak,
            hardness,
            perception,
            concealment,
            resistances AS "resistances!: serde_json::Value",
            ward_percent,
            wealth,
            class_id,
            behaviors AS "behaviors!: Vec<MobBehavior>",
            protected_kind AS "protected_kind!: ProtectedKind",
            professions AS "professions!: Vec<MobProfession>",
            gender::text AS "gender!",
            race::text AS "race!",
            size AS "size!: Size",
            "lifeForce" AS "life_force!: LifeForce",
            "damageType" AS "damage_type!: DamageType",
            "move" AS "move_points!",
            "defaultPosition" AS "default_position!: Position",
            traits AS "traits!: Vec<MobTrait>",
            movement_mode AS "movement_mode!: MovementMode",
            default_movement_mode AS "default_movement_mode!: MovementMode",
            aggression_formula
        FROM "Mobs"
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
