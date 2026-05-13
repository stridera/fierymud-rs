use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::enums::Sector;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub zone_id: i32,
    pub id: i32,
    pub name: String,
    pub room_description: String,
    pub sector: Sector,
    pub base_light_level: i32,
    pub capacity: i32,
    /// Schema's `is_peaceful` flag — combat is refused in these
    /// rooms (sanctuaries, shop interiors, quest hubs). Loaded
    /// into a `PeacefulRoom` marker on the spawned room entity.
    pub is_peaceful: bool,
    /// `Room.allows_magic` — when false, spells/chants/songs
    /// fizzle in this room (dead-magic zones, anti-magic shrines).
    /// Loaded into a `NoMagicRoom` marker; the loader inverts the
    /// schema polarity so the marker is present only where magic
    /// is forbidden, matching the cost of an absent component for
    /// the common case.
    pub allows_magic: bool,
    /// `Room.allows_recall` — when false, `recall` / `home` refuses
    /// with "some power blocks your recall." Sanctuary inversion:
    /// builders mark unstable / planar / hostile zones to keep
    /// recall from short-circuiting them. Stored as `NoRecallRoom`.
    pub allows_recall: bool,
    /// `Room.allows_summon` — when false, `summon` (player-to-player
    /// or staff mob-spawn) refuses with a flavor refusal. Stored as
    /// `NoSummonRoom`.
    pub allows_summon: bool,
    /// `Room.allows_teleport` — when false, `teleport` / staff
    /// `goto` refuses with the destination as the gate (not the
    /// origin), mirroring legacy parity. Stored as `NoTeleportRoom`.
    pub allows_teleport: bool,
    /// `Room.is_death_trap` — entering immediately kills the player.
    /// Classic CircleMUD DT semantics. Stored as `DeathTrap`.
    pub is_death_trap: bool,
    /// `Room.is_indoors` — suppresses weather/sky rendering and
    /// flags the room as sheltered for sector-based heuristics.
    /// Stored as `IndoorRoom`.
    pub is_indoors: bool,
    /// `Room.is_soundproof` — global channels (shout / gossip /
    /// music / quest-say) skip delivery to occupants of this room.
    /// Stored as `SoundproofRoom`.
    pub is_soundproof: bool,
    /// `Room.is_arena` — combat is welcome here regardless of PK
    /// consent. Stored as `ArenaRoom`. Useful tag in `look`.
    pub is_arena: bool,
    /// `Room.is_guildhall` — class trainers gate visits here; also
    /// a visible tag in `look`. Stored as `GuildhallRoom`.
    pub is_guildhall: bool,
    /// `Room.allows_mobs` — when false, wandering mobs refuse to
    /// enter. Stored as `NoMobsRoom` (negative polarity to match
    /// the rest of the `No*Room` markers).
    pub allows_mobs: bool,
    /// `Room.allows_tracking` — when false, `track` refuses to
    /// route through this room. Stored as `NoTrackingRoom`.
    pub allows_tracking: bool,
    /// `Room.allows_portals` — when false, portal/moonwell creation
    /// refuses to link to/from this room. Stored as `NoPortalsRoom`.
    pub allows_portals: bool,
    /// `Room.allows_scanning` — when false, `scan` cannot peer in
    /// or out of this room. Stored as `NoScanningRoom`.
    pub allows_scanning: bool,
    /// Builder-authored mapper coordinates. Nullable per axis;
    /// when any are set the runtime threads them through to the
    /// `Room.Info` GMCP frame so client-side mappers (Mudlet,
    /// MUSHclient) can place rooms exactly where the builder
    /// laid them out instead of falling back to compass-walk
    /// auto-placement. Z defaults to 0 in the schema; X/Y are
    /// genuinely Option (rooms imported from legacy sources
    /// without coordinates carry NULL).
    pub layout_x: Option<i32>,
    pub layout_y: Option<i32>,
    pub layout_z: Option<i32>,
}

pub async fn list_rooms(pool: &PgPool) -> sqlx::Result<Vec<Room>> {
    sqlx::query_as!(
        Room,
        r#"
        SELECT
            zone_id,
            id,
            name,
            room_description,
            sector AS "sector: Sector",
            base_light_level,
            capacity,
            is_peaceful,
            allows_magic,
            allows_recall,
            allows_summon,
            allows_teleport,
            is_death_trap,
            is_indoors,
            is_soundproof,
            is_arena,
            is_guildhall,
            allows_mobs,
            allows_tracking,
            allows_portals,
            allows_scanning,
            layout_x,
            layout_y,
            layout_z
        FROM "Room"
        WHERE deleted_at IS NULL
        ORDER BY zone_id, id
        "#
    )
    .fetch_all(pool)
    .await
}
