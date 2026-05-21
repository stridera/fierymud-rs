//! Camp lifecycle: setup, tick, completion. The legacy `do_camp`
//! pattern was a "safe wilderness logout" — pitch a tent, wait,
//! save, disconnect. The Rust port treats it as a "long rest with
//! checkpoint" instead: the auto-save-on-disconnect path already
//! covers logout safety, but a structured rest that takes a
//! commitment of in-game time and ends with a save is still
//! useful (and matches the legacy outdoors-only restriction the
//! user called out).
//!
//! Rest / repose (R2): on completion, the camp also acquires a
//! `CAMP` **RestSource** with a tier computed from the player's
//! class, group composition, and the optional kit. The kit (if any)
//! is consumed at completion only; mid-camp cancels leave the kit
//! intact per the design doc's edge-case table.

use bevy_ecs::prelude::*;
use mud_db::enums::{RestSource, Sector};
use mud_world::{
    Camping, Fighting, Item, Located, PendingSave, PendingWakeAttachments, Profile, RestState,
};

use crate::TickCount;
use crate::commands::{broadcast_room_except_players_rendered, name_of, send_rendered};

/// Sectors a player may camp in. Mirrors the legacy refusal
/// pattern: indoor (Structure), city, water variants, and air
/// are all out. Caves / underdark / planes are also refused —
/// not "outdoors" in any meaningful sense.
#[must_use]
pub fn sector_allows_camp(sector: Sector) -> bool {
    matches!(
        sector,
        Sector::Field
            | Sector::Forest
            | Sector::Hills
            | Sector::Mountain
            | Sector::Road
            | Sector::Grasslands
            | Sector::Beach
            | Sector::Swamp
            | Sector::Ruins
    )
}

/// Ticks elapsed before camp completes. At 10Hz this is 35 real
/// seconds — short enough to feel responsive, long enough that
/// camping in unsafe terrain still mattered if a wandering mob
/// shows up.
pub const CAMP_DURATION_TICKS: u64 = 350;

/// Rest / repose tier-3 cap. The camp tier computation clamps to
/// this; matches the `restTier` schema range (1..=3).
const CAMP_TIER_MAX: i32 = 3;

/// Class IDs whose holders self-grant the "fieldcraft" bonus in
/// `compute_camp_tier` (Ranger + Druid in the legacy class table).
/// **TUNABLE** scaffolding per the design doc — R8 follow-up
/// migrates this to a `Class.campcraftBonus` DB column read.
const CAMPCRAFT_CLASS_IDS: &[i32] = &[
    // Ranger
    9, // Druid
    7,
];

/// Compute the rest tier earned at camp completion. Mirrors the
/// design doc §"Camp tier computation":
///
/// ```text
/// tier = 1
/// if class is Ranger/Druid OR a party member is Ranger/Druid:
///     tier += 1     (fieldcraft bonus, not double-counted)
/// if kit was consumed:
///     tier += kit.camp_kit_tier   (1 basic, 2 premium)
/// clamp tier to 3
/// ```
///
/// Today we don't have a runtime party concept beyond the follow-
/// chain `group_members` helper, so the "party member is
/// Ranger/Druid" check walks the follow root and inspects each
/// member's `Profile.class_id` for membership in
/// [`CAMPCRAFT_CLASS_IDS`]. When the party API hardens this is
/// the right place to widen the check.
fn compute_camp_tier(world: &mut World, player: Entity, kit_tier_bonus: i32) -> i32 {
    let mut tier = 1;
    let root = crate::commands::group_root(world, player);
    let members = crate::commands::group_members(world, root);
    let fieldcraft = members.iter().any(|m| {
        world
            .get::<Profile>(*m)
            .and_then(|p| p.class_id)
            .is_some_and(|cid| CAMPCRAFT_CLASS_IDS.contains(&cid))
    });
    if fieldcraft {
        tier += 1;
    }
    tier += kit_tier_bonus.max(0);
    tier.min(CAMP_TIER_MAX)
}

/// Per-tick walk over `Camping` players: cancels on combat or room
/// movement, completes when the deadline is reached. Completion
/// is a no-op gameplay-wise; the player just gets a flavor line
/// and a `PendingSave` marker so the next save loop checkpoints
/// them. Movement / combat aborts also clear the `Camping`
/// component.
pub fn camp_tick(world: &mut World) {
    let now_tick = world.resource::<TickCount>().0;
    let snapshot: Vec<(Entity, Camping)> = {
        let mut q = world.query::<(Entity, &Camping)>();
        q.iter(world)
            .map(|(e, c)| (e, *c))
            .collect()
    };
    for (entity, camp) in snapshot {
        // Combat-cancel: mid-camp ambush wakes you up.
        if world.get::<Fighting>(entity).is_some() {
            cancel(
                world,
                entity,
                "Combat shatters your half-finished camp.",
            );
            continue;
        }
        // Movement-cancel: leaving the campsite ends it.
        let now_in = world.get::<Located>(entity).map(|l| l.0);
        if now_in != Some(camp.started_in) {
            cancel(
                world,
                entity,
                "You wander away from your half-pitched campsite.",
            );
            continue;
        }
        // Completion: deadline reached.
        if now_tick.saturating_sub(camp.since_tick) >= CAMP_DURATION_TICKS {
            complete(world, entity, camp);
        }
    }
}

fn cancel(world: &mut World, entity: Entity, msg: &str) {
    // Rest / repose: design doc edge case — "Camp interrupted (combat
    // / movement during 35s setup): Kit NOT consumed. restSource
    // unchanged." So we just drop the Camping component; the kit
    // remains in the player's inventory.
    if let Ok(mut em) = world.get_entity_mut(entity) {
        em.remove::<Camping>();
    }
    send_rendered(world, entity, &format!("{msg}\r\n"));
}

fn complete(world: &mut World, entity: Entity, camp: Camping) {
    // Consume the kit first, BEFORE clearing the Camping component,
    // so a despawn during item-removal doesn't strand us with a
    // half-applied state.
    if let Some(kit) = camp.kit_entity
        && world.get_entity(kit).is_ok()
        && world.get::<Item>(kit).is_some()
        && let Ok(em) = world.get_entity_mut(kit)
    {
        em.despawn();
    }
    // Compute the rest tier from class + group composition + kit.
    let tier = compute_camp_tier(world, entity, camp.kit_tier_bonus);
    // Preserve any existing Repose; acquisition only overwrites the
    // source + tier per the design doc ("Pool is NEVER cleared by
    // acquisition — only by XP-gain consumption.").
    let existing_repose = world.get::<RestState>(entity).map_or(0, |r| r.repose);
    if let Ok(mut em) = world.get_entity_mut(entity) {
        em.remove::<Camping>();
        em.insert(PendingSave);
        em.insert(RestState {
            repose: existing_repose,
            source: RestSource::Camp,
            tier,
        });
        if let Some((zone, id)) = camp.kit_world_key {
            em.insert(PendingWakeAttachments { kit_zone: zone, kit_id: id });
        }
    }
    let player_name = name_of(world, entity);
    let room = world.get::<Located>(entity).map(|l| l.0);
    send_rendered(
        world,
        entity,
        "<b:cyan>You complete your campsite, settle in, and rest for a while.</>\r\n",
    );
    if let Some(room) = room {
        broadcast_room_except_players_rendered(
            world,
            room,
            &[entity],
            &format!("<dim>{player_name} finishes pitching camp and settles in.</>\r\n"),
        );
    }
}
