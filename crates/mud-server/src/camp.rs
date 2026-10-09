//! Camp lifecycle: setup, tick, completion. Mirrors the legacy
//! `do_camp`: pitch a tent in a campable room, wait out the
//! countdown, and on completion the character is saved and logged
//! out through the same path as `quit` (`Quitting`, drained by
//! `ConnRouter::drain_quitting`). Moving, fighting, or being
//! attacked during the countdown cancels it. No penalty either way.
//!
//! Rest / repose (R2): on completion, the camp also acquires a
//! `CAMP` **`RestSource`** with a tier computed from the player's
//! class, group composition, and the optional kit. The kit (if any)
//! is consumed at completion only; mid-camp cancels leave the kit
//! intact per the design doc's edge-case table.

use bevy_ecs::prelude::*;
use mud_db::enums::{RestSource, Sector};
use mud_world::{Camping, ClassCatalog, Item, Located, PendingWakeAttachments, Profile, RestState};

use crate::TickCount;
use crate::commands::{Camped, Quitting, send_rendered};

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

/// Compute the rest tier earned at camp completion. Mirrors the
/// design doc §"Camp tier computation":
///
/// ```text
/// tier = 1
/// if class has Class.campcraft_bonus OR a party member's class does:
///     tier += 1     (fieldcraft bonus, not double-counted)
/// if kit was consumed:
///     tier += kit.camp_kit_tier   (1 basic, 2 premium)
/// clamp tier to 3
/// ```
///
/// The "party member is Ranger/Druid" check walks the player's real group
/// (`invite` + `accept`, not the follow tree) and inspects each member's
/// `Profile.class_id` against the `Class.campcraft_bonus` flag in the
/// [`ClassCatalog`] (Ranger and Druid in the seeded data).
fn compute_camp_tier(world: &mut World, player: Entity, kit_tier_bonus: i32) -> i32 {
    let mut tier = 1;
    let root = crate::commands::group_root(world, player);
    let members = crate::commands::group_members(world, root);
    let fieldcraft = members.iter().any(|m| {
        let class_id = world.get::<Profile>(*m).and_then(|p| p.class_id);
        class_id.is_some_and(|cid| {
            world
                .get_resource::<ClassCatalog>()
                .and_then(|c| c.by_id.get(&cid))
                .is_some_and(|c| c.campcraft_bonus)
        })
    });
    if fieldcraft {
        tier += 1;
    }
    tier += kit_tier_bonus.max(0);
    tier.min(CAMP_TIER_MAX)
}

/// Per-tick walk over `Camping` players: cancels on combat or room
/// movement, completes when the deadline is reached. Completion
/// stamps the `Camp` rest source and flags the player `Quitting`
/// so the main loop saves and disconnects them. Movement / combat
/// aborts clear the `Camping` component.
pub fn camp_tick(world: &mut World) {
    let now_tick = world.resource::<TickCount>().0;
    let snapshot: Vec<(Entity, Camping)> = {
        let mut q = world.query::<(Entity, &Camping)>();
        q.iter(world).map(|(e, c)| (e, *c)).collect()
    };
    for (entity, camp) in snapshot {
        // Combat-cancel: mid-camp ambush wakes you up.
        if crate::commands::in_combat(world, entity) {
            cancel(
                world,
                entity,
                "You decide now is not the best time for camping.",
            );
            continue;
        }
        // Movement-cancel: leaving the campsite ends it.
        let now_in = world.get::<Located>(entity).map(|l| l.0);
        if now_in != Some(camp.started_in) {
            cancel(
                world,
                entity,
                "You are no longer near where you began the campsite.",
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
        em.insert(Quitting);
        em.insert(Camped);
        em.insert(RestState {
            repose: existing_repose,
            source: RestSource::Camp,
            tier,
        });
        if let Some((zone, id)) = camp.kit_world_key {
            em.insert(PendingWakeAttachments {
                kit_zone: zone,
                kit_id: id,
            });
        }
    }
    // The room hears the departure from `retire_player` once the player is
    // saved and removed.
    send_rendered(
        world,
        entity,
        "<b:cyan>You complete your campsite, and leave this world for a while.</>\r\n",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_world::{Fighting, Health, Named, Player};

    fn camper_in(world: &mut World, room: Entity, since_tick: u64) -> Entity {
        world
            .spawn((
                Player,
                Named {
                    name: "Camper".into(),
                },
                Located(room),
                Health { hp: 10, max: 10 },
                Camping {
                    since_tick,
                    started_in: room,
                    kit_entity: None,
                    kit_world_key: None,
                    kit_tier_bonus: 0,
                },
            ))
            .id()
    }

    fn world_at(tick: u64) -> World {
        let mut world = World::new();
        world.insert_resource(TickCount(tick));
        world
    }

    fn class(id: i32, plain_name: &str, campcraft_bonus: bool) -> mud_world::ClassDef {
        mud_world::ClassDef {
            id,
            name: plain_name.into(),
            plain_name: plain_name.into(),
            is_subclass: false,
            parent_class_id: None,
            description: None,
            hit_dice: "1d8".into(),
            primary_stat: None,
            hp_per_level: 10,
            exp_gain_factor: 1.0,
            alignment_bias: 0,
            campcraft_bonus,
            resistances: std::collections::HashMap::new(),
        }
    }

    fn player_of_class(world: &mut World, class_id: i32) -> Entity {
        world
            .spawn((
                Player,
                Profile {
                    level: 10,
                    class_id: Some(class_id),
                    race: "Human".into(),
                    experience: 0,
                    gender: "neutral".into(),
                },
            ))
            .id()
    }

    #[test]
    fn camp_tier_reads_the_class_catalog_flag() {
        // Ids and flags as seeded in fierydev: Druid is 8 and gets the bonus,
        // Shaman is 9 and does not (the old hard-coded list had them swapped).
        let mut world = World::new();
        let mut catalog = ClassCatalog::default();
        for c in [
            class(7, "Ranger", true),
            class(8, "Druid", true),
            class(9, "Shaman", false),
        ] {
            catalog.by_id.insert(c.id, c);
        }
        world.insert_resource(catalog);
        let druid = player_of_class(&mut world, 8);
        let shaman = player_of_class(&mut world, 9);
        assert_eq!(compute_camp_tier(&mut world, druid, 0), 2);
        assert_eq!(compute_camp_tier(&mut world, shaman, 0), 1);
        // Clamped at the tier cap with a premium kit.
        assert_eq!(compute_camp_tier(&mut world, druid, 5), CAMP_TIER_MAX);
    }

    #[test]
    fn camp_tier_without_a_catalog_has_no_bonus() {
        let mut world = World::new();
        let druid = player_of_class(&mut world, 8);
        assert_eq!(compute_camp_tier(&mut world, druid, 0), 1);
    }

    #[test]
    fn countdown_completes_into_a_camp_logout() {
        let mut world = world_at(CAMP_DURATION_TICKS - 1);
        let room = world.spawn_empty().id();
        let camper = camper_in(&mut world, room, 0);
        camp_tick(&mut world);
        assert!(world.get::<Camping>(camper).is_some());
        assert!(world.get::<Quitting>(camper).is_none());

        world.insert_resource(TickCount(CAMP_DURATION_TICKS));
        camp_tick(&mut world);
        assert!(world.get::<Camping>(camper).is_none());
        assert!(world.get::<Quitting>(camper).is_some());
        assert!(world.get::<Camped>(camper).is_some());
        assert_eq!(
            world.get::<RestState>(camper).unwrap().source,
            RestSource::Camp
        );
    }

    #[test]
    fn moving_cancels_the_countdown() {
        let mut world = world_at(10);
        let room = world.spawn_empty().id();
        let elsewhere = world.spawn_empty().id();
        let camper = camper_in(&mut world, room, 0);
        world.entity_mut(camper).insert(Located(elsewhere));
        world.insert_resource(TickCount(CAMP_DURATION_TICKS + 10));
        camp_tick(&mut world);
        // Cancelled, not completed, even though the deadline had passed.
        assert!(world.get::<Camping>(camper).is_none());
        assert!(world.get::<Quitting>(camper).is_none());
    }

    #[test]
    fn being_attacked_cancels_the_countdown() {
        let mut world = world_at(10);
        let room = world.spawn_empty().id();
        let camper = camper_in(&mut world, room, 0);
        let mob = world.spawn((mud_world::Mob, Located(room))).id();
        world.entity_mut(camper).insert(Fighting(mob));
        world.insert_resource(TickCount(CAMP_DURATION_TICKS + 10));
        camp_tick(&mut world);
        assert!(world.get::<Camping>(camper).is_none());
        assert!(world.get::<Quitting>(camper).is_none());
    }
}
