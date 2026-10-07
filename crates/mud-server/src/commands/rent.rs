//! `rent`: leave the world at a receptionist, or book a prepaid inn rest.
//!
//! - `rent` with no argument while a receptionist (a mob whose prototype
//!   carries the `Receptionist` profession) stands in the room: legacy
//!   "store your belongings" lines, then exactly what `quit` does
//!   ([`begin_quit`]: the `Quitting` marker drained by the connection layer
//!   into the ordered save + disconnect). Any rest source already booked
//!   (inn tier, camp) is kept by that save path, so a prepaid rest is
//!   applied on the way out.
//! - `rent` with no argument in an `is_inn` room with no receptionist:
//!   print the available tier menu from the room's `InnRoom` component.
//! - `rent <tier-name>` in an `is_inn` room: validate the name, check
//!   gold, and either set the `RestSource` to `Inn` immediately (tier 1)
//!   or stash a `PendingRentConfirm` component and prompt the player to
//!   confirm (tier > 1). The player then `rent`s (or `quit`s) to leave.
//! - Anywhere else: "There's nothing to rent here." and nothing happens.
//!
//! Per ADR 0001 §3 the fee is **flat per tier**, NOT per-night. The
//! player who returns in a year pays the same as the player who
//! returns in an hour. Per the design doc's edge-case table,
//! renting downgrade after an existing higher-tier rent overwrites
//! the source with no refund — caveat emptor.

use bevy_ecs::prelude::*;
use mud_db::enums::{RestSource, UserRole};
use mud_world::{
    Account, Fighting, InnRoom, Located, Mob, MobPrototypes, Posture, PostureKind, RestState,
    Stunned, Wealth, WorldKey,
};

use crate::commands::info::begin_quit;
use crate::commands::{Category, Command, Help, name_of, send_rendered, send_to};

/// Copper-per-gold conversion. Wealth is stored in copper; inn
/// `fee_gp` is authored in gold pieces. Centralized so a future
/// denomination tweak doesn't drift between the `rent` price line,
/// the deduct call, and the affordability check.
const COPPER_PER_GOLD: i64 = 100;

inventory::submit! {
    Command {
        names: &["rent"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "rent [<tier-name>]",
            summary: "Rent a room: save and leave at a receptionist, or book an inn rest.",
            long: "With a receptionist present, plain `rent` stores \
                   your belongings and leaves the game exactly like \
                   `quit`, keeping any rest you have already booked. \
                   `offer` lists the available inn tiers and their \
                   prices (so does `rent` in an inn with no \
                   receptionist). `rent <name>` books that tier and \
                   charges the fee in \
                   gold and queues an INN RestSource at the chosen \
                   tier; you'll see Refreshed regen and any Wake \
                   Effect attachments on your next XP gain after \
                   logging back in. Tiers 2 and 3 prompt for \
                   confirmation; reply `y` to confirm or `n` to \
                   abort. Fee is flat — pay once, return whenever.",
        },
        run: cmd_rent,
    }
}

inventory::submit! {
    Command {
        names: &["offer"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Settings,
        help: Help {
            usage: "offer",
            summary: "Ask a receptionist what it costs to store your belongings.",
            long: "Speak to a receptionist (or stand in an inn) to hear \
                   the terms. Your character and belongings are saved \
                   automatically and cost nothing to keep: `quit` \
                   anywhere (or `rent` at a receptionist) to save and leave. Inns additionally sell \
                   prepaid rest tiers; `offer` lists them, `rent <name>` \
                   books one.",
        },
        run: cmd_offer,
    }
}

/// Set on the player when `rent <tier>` matched a tier in 1..=3
/// pending the y/n confirmation step. Carries the resolved tier
/// index, name, and fee so the confirm handler doesn't need to
/// re-walk the inn's menu.
#[derive(Component, Debug, Clone)]
pub(crate) struct PendingRentConfirm {
    pub tier_name: String,
    pub tier: i32,
    pub fee_gp: i32,
}

pub(crate) fn cmd_rent(world: &mut World, player: Entity, args: &str) {
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;
    let clerk = receptionist_in_room(world, room);
    let inn = world.get::<InnRoom>(room).cloned();
    // `rent ` (a trailing space: a client sending an empty tier name) is an
    // argument that happens to be empty, not a bare `rent`, and never quits.
    let bare = args.trim().is_empty() && !crate::commands::line_has_trailing_space();
    if bare && let Some(clerk) = clerk {
        rent_and_quit(world, player, room, clerk);
        return;
    }
    let Some(inn) = inn else {
        if let Some(clerk) = clerk {
            tell_rent_is_free(world, player, clerk);
        } else {
            send_to(world, player, "There's nothing to rent here.\r\n");
        }
        return;
    };
    let arg = args.trim();
    if arg.is_empty() {
        render_tier_menu(world, player, &inn);
        return;
    }
    // Resolve tier name (case-insensitive prefix match).
    let needle = arg.to_ascii_lowercase();
    let chosen = inn
        .tiers
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(&needle))
        .or_else(|| {
            inn.tiers
                .iter()
                .find(|t| t.name.to_ascii_lowercase().starts_with(&needle))
        })
        .cloned();
    let Some(chosen) = chosen else {
        send_to(
            world,
            player,
            format!("'{arg}' isn't one of the available rooms here.\r\n"),
        );
        return;
    };
    // Affordability check.
    let on_hand = world.get::<Wealth>(player).map_or(0, |w| w.0);
    let fee_copper = i64::from(chosen.fee_gp).saturating_mul(COPPER_PER_GOLD);
    if on_hand < fee_copper {
        send_to(
            world,
            player,
            format!(
                "You can't afford the {} ({} gp).\r\n",
                chosen.name, chosen.fee_gp
            ),
        );
        return;
    }
    // Tier > 1 requires a confirm prompt; tier 1 is fire-and-forget.
    if chosen.tier > 1 {
        if let Ok(mut em) = world.get_entity_mut(player) {
            em.insert(PendingRentConfirm {
                tier_name: chosen.name.clone(),
                tier: chosen.tier,
                fee_gp: chosen.fee_gp,
            });
        }
        send_to(
            world,
            player,
            format!(
                "Renting the {} for {}gp. Confirm? (y/n)\r\n",
                chosen.name, chosen.fee_gp
            ),
        );
        return;
    }
    finalize_rent(world, player, &chosen.name, chosen.tier, chosen.fee_gp);
}

/// First receptionist mob standing in `room`, identified by the
/// `Receptionist` profession on its prototype.
fn receptionist_in_room(world: &mut World, room: Entity) -> Option<Entity> {
    // Scan only the room's `Contents` index, not every mob in the world.
    let mobs: Vec<(Entity, WorldKey)> = world
        .get::<mud_world::Contents>(room)?
        .iter()
        .filter(|e| world.get::<Mob>(*e).is_some())
        .filter_map(|e| world.get::<WorldKey>(e).map(|k| (e, *k)))
        .collect();
    let protos = world.get_resource::<MobPrototypes>()?;
    mobs.into_iter().find_map(|(e, k)| {
        protos
            .by_key
            .get(&(k.zone, k.id))
            .filter(|p| {
                p.professions
                    .contains(&mud_db::enums::MobProfession::Receptionist)
            })
            .map(|_| e)
    })
}

/// Legacy receptionist `rent`: the clerk takes the player's things and
/// shows them to their chamber, then the player leaves via the shared
/// `quit` path. Nothing is said to the room unless the quit began (it can be
/// refused, e.g. mid-fight).
fn rent_and_quit(world: &mut World, player: Entity, room: Entity, clerk: Entity) {
    let clerk_name = crate::commands::cap_sentence_start(&name_of(world, clerk));
    if !clerk_can_serve(world, player, clerk, &clerk_name) {
        return;
    }
    let farewell = format!(
        "<b:white>{clerk_name} tells you, 'Rent?  Sure, come this way!'</>\r\n\
         <b:white>{clerk_name} stores your belongings and helps you into your private chamber.</>\r\n"
    );
    if !begin_quit(world, player, &farewell) {
        return;
    }
    let player_name = name_of(world, player);
    crate::commands::broadcast_room_visual(
        world,
        room,
        player,
        &[player],
        &crate::commands::cap_sentence_start(&format!(
            "{clerk_name} helps {player_name} into their private chamber.\r\n"
        )),
    );
}

/// Legacy `gen_receptionist` gates: the clerk must be awake, free and able
/// to see the player (staff are exempt from the sight check). Sends the
/// refusal itself.
fn clerk_can_serve(world: &mut World, player: Entity, clerk: Entity, clerk_name: &str) -> bool {
    let asleep = world
        .get::<Posture>(clerk)
        .is_some_and(|p| p.0.rank() <= PostureKind::Sleeping.rank());
    if asleep || world.get::<Stunned>(clerk).is_some() {
        send_to(
            world,
            player,
            format!("{clerk_name} is unable to talk to you...\r\n"),
        );
        return false;
    }
    if world.get::<Fighting>(clerk).is_some() {
        send_to(
            world,
            player,
            format!("{clerk_name} is too busy to help you right now.\r\n"),
        );
        return false;
    }
    let is_staff = world
        .get::<Account>(player)
        .is_some_and(|a| a.role.rank() > UserRole::Player.rank());
    if !is_staff && !crate::commands::can_see_player(world, clerk, player) {
        send_rendered(
            world,
            player,
            &format!("{clerk_name} says, 'I don't deal with people I can't see!'\r\n"),
        );
        return false;
    }
    true
}

/// The receptionist's answer to `rent <something>` where there are no rooms
/// to book: storage is free, plain `rent` (or `quit`) saves.
fn tell_rent_is_free(world: &mut World, player: Entity, clerk: Entity) {
    let clerk_name = crate::commands::cap_sentence_start(&name_of(world, clerk));
    send_rendered(
        world,
        player,
        &format!(
            "{clerk_name} tells you, 'No rent is due here. Your belongings keep for free. \
             Just type <b:white>rent</> whenever you are ready to leave.'\r\n"
        ),
    );
}

/// `offer`: what the receptionist will do for you. Reports that
/// storage is free and, in an inn, lists the prepaid rest tiers.
pub(crate) fn cmd_offer(world: &mut World, player: Entity, _args: &str) {
    let Some(located) = world.get::<Located>(player).copied() else {
        send_to(world, player, "You are nowhere.\r\n");
        return;
    };
    let room = located.0;
    let clerk = receptionist_in_room(world, room);
    let inn = world.get::<InnRoom>(room).cloned();
    if clerk.is_none() && inn.is_none() {
        send_to(
            world,
            player,
            "There's no one here to make you an offer.\r\n",
        );
        return;
    }
    if let Some(clerk) = clerk {
        tell_rent_is_free(world, player, clerk);
    } else {
        send_to(
            world,
            player,
            "No rent is due: your belongings are saved automatically. Type quit to leave.\r\n",
        );
    }
    if let Some(inn) = inn {
        render_tier_menu(world, player, &inn);
    }
}

/// Apply the rent: deduct gold, set RestSource=Inn, restTier=chosen.
/// Preserves any existing Repose pool per the design doc
/// ("Pool is NEVER cleared by acquisition — only by XP-gain
/// consumption.").
pub(crate) fn finalize_rent(
    world: &mut World,
    player: Entity,
    tier_name: &str,
    tier: i32,
    fee_gp: i32,
) {
    let fee_copper = i64::from(fee_gp).saturating_mul(COPPER_PER_GOLD);
    let on_hand = world.get::<Wealth>(player).map_or(0, |w| w.0);
    if on_hand < fee_copper {
        send_to(
            world,
            player,
            format!("You can't afford the {tier_name} ({fee_gp} gp).\r\n"),
        );
        return;
    }
    if let Some(mut w) = world.get_mut::<Wealth>(player) {
        w.0 = w.0.saturating_sub(fee_copper);
    }
    let existing_repose = world.get::<RestState>(player).map_or(0, |r| r.repose);
    if let Ok(mut em) = world.get_entity_mut(player) {
        em.insert(RestState {
            repose: existing_repose,
            source: RestSource::Inn,
            tier,
        });
    }
    send_rendered(
        world,
        player,
        &format!(
            "<b:cyan>You rent the {tier_name} ({fee_gp} gp). Your rest is prepaid until your next XP gain.</>\r\n"
        ),
    );
    // Room broadcast — renting a room at an inn is a visible
    // transaction at the counter; party-mates following the
    // renter into the inn should see who chose which tier.
    if let Some(located) = world.get::<Located>(player).copied() {
        let player_name = crate::commands::name_of(world, player);
        crate::commands::broadcast_room_visual(
            world,
            located.0,
            player,
            &[player],
            &crate::commands::cap_sentence_start(&format!(
                "{player_name} hands over {fee_gp} gp and books the {tier_name}.\r\n"
            )),
        );
    }
}

fn render_tier_menu(world: &mut World, player: Entity, inn: &InnRoom) {
    if inn.tiers.is_empty() {
        send_to(
            world,
            player,
            format!("Available rooms here at {}: (none)\r\n", inn.inn_name),
        );
        return;
    }
    let mut out = format!("Available rooms here at {}:\r\n", inn.inn_name);
    for t in &inn.tiers {
        out.push_str(&format!(
            "  {:<14} tier {}   {} gp\r\n",
            t.name, t.tier, t.fee_gp
        ));
    }
    out.push_str("`offer` lists prices; `rent <name>` books a room (tiers 2 and 3 ask to confirm). With a receptionist present, plain `rent` stores your belongings and leaves the game.\r\n");
    send_to(world, player, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{self, drain};
    use mud_world::{InnTier, MobProto};

    fn world_with_clerk(inn: bool) -> (World, Entity, test_support::Rx) {
        let mut world = World::new();
        let mut protos = MobPrototypes::default();
        let proto: MobProto =
            test_support::mob_proto(1, 5, mud_db::enums::MobProfession::Receptionist);
        protos.by_key.insert((1, 5), proto);
        world.insert_resource(protos);
        let room = world.spawn_empty().id();
        if inn {
            world.entity_mut(room).insert(InnRoom {
                inn_name: "The Inn".into(),
                tiers: vec![InnTier {
                    name: "basic".into(),
                    tier: 1,
                    fee_gp: 5,
                }],
            });
        }
        world.spawn((
            Mob,
            WorldKey { zone: 1, id: 5 },
            Located(room),
            mud_world::Named {
                name: "the receptionist".into(),
            },
        ));
        let (player, rx) = test_support::player_in(&mut world, room);
        (world, player, rx)
    }

    #[test]
    fn receptionist_lookup_is_scoped_to_the_room() {
        let (mut world, player, _rx) = world_with_clerk(false);
        let here = world.get::<Located>(player).unwrap().0;
        let clerk = receptionist_in_room(&mut world, here).expect("clerk in this room");
        assert!(world.get::<Mob>(clerk).is_some());
        // A receptionist in another room is not found from here.
        let elsewhere = world.spawn_empty().id();
        assert!(receptionist_in_room(&mut world, elsewhere).is_none());
        // A room with no contents index at all is fine too.
        let empty = world.spawn_empty().id();
        assert!(receptionist_in_room(&mut world, empty).is_none());
    }

    #[test]
    fn rent_at_a_receptionist_stores_belongings_and_quits() {
        let (mut world, player, mut rx) = world_with_clerk(false);
        cmd_rent(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("Rent?  Sure, come this way!"), "{out}");
        assert!(
            out.contains("stores your belongings and helps you into your private chamber"),
            "{out}"
        );
        // Same marker `quit` sets: the connection layer then runs the
        // ordered save + disconnect.
        assert!(world.get::<crate::commands::Quitting>(player).is_some());
    }

    #[test]
    fn rent_sets_the_same_state_as_quit() {
        let (mut world, player, _rx) = world_with_clerk(false);
        crate::commands::info::cmd_quit(&mut world, player, "");
        let quit_marker = world.get::<crate::commands::Quitting>(player).is_some();
        world
            .entity_mut(player)
            .remove::<crate::commands::Quitting>();
        cmd_rent(&mut world, player, "");
        assert_eq!(
            world.get::<crate::commands::Quitting>(player).is_some(),
            quit_marker
        );
    }

    #[test]
    fn rent_at_a_receptionist_keeps_a_booked_rest() {
        let (mut world, player, _rx) = world_with_clerk(true);
        world.entity_mut(player).insert(RestState {
            repose: 7,
            source: RestSource::Inn,
            tier: 2,
        });
        cmd_rent(&mut world, player, "");
        assert!(world.get::<crate::commands::Quitting>(player).is_some());
        let rest = world.get::<RestState>(player).unwrap();
        assert_eq!(
            (rest.source, rest.tier, rest.repose),
            (RestSource::Inn, 2, 7)
        );
    }

    #[test]
    fn rent_at_a_receptionist_is_refused_mid_fight() {
        let (mut world, player, mut rx) = world_with_clerk(false);
        let room = world.get::<Located>(player).unwrap().0;
        let foe = world.spawn((Mob, Located(room))).id();
        world.entity_mut(player).insert(mud_world::Fighting(foe));
        cmd_rent(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("fighting for your life"), "{out}");
        assert!(!out.contains("private chamber"), "{out}");
        assert!(world.get::<crate::commands::Quitting>(player).is_none());
    }

    #[test]
    fn rent_with_a_tier_name_and_no_inn_does_not_quit() {
        let (mut world, player, mut rx) = world_with_clerk(false);
        cmd_rent(&mut world, player, "basic");
        let out = drain(&mut rx);
        assert!(out.contains("No rent is due"), "{out}");
        assert!(world.get::<crate::commands::Quitting>(player).is_none());
    }

    #[test]
    fn rent_in_an_inn_without_a_receptionist_lists_tiers_and_does_not_quit() {
        let (mut world, player, mut rx) = world_with_clerk(true);
        let room = world.get::<Located>(player).unwrap().0;
        let clerk = receptionist_in_room(&mut world, room).unwrap();
        world.despawn(clerk);
        cmd_rent(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(
            out.contains("Available rooms here") && out.contains("basic"),
            "{out}"
        );
        assert!(world.get::<crate::commands::Quitting>(player).is_none());
    }

    fn clerk_of(world: &mut World, player: Entity) -> Entity {
        let room = world.get::<Located>(player).unwrap().0;
        receptionist_in_room(world, room).unwrap()
    }

    #[test]
    fn rent_with_a_trailing_space_is_an_empty_argument_and_does_not_quit() {
        let (mut world, player, mut rx) = world_with_clerk(false);
        world.insert_resource(mud_world::SocialRegistry::default());
        world.entity_mut(player).insert(Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        });
        crate::commands::dispatch(&mut world, player, "rent ");
        let out = drain(&mut rx);
        assert!(out.contains("No rent is due"), "{out}");
        assert!(world.get::<crate::commands::Quitting>(player).is_none());
        // The bare form on the same world still quits.
        crate::commands::dispatch(&mut world, player, "rent");
        assert!(world.get::<crate::commands::Quitting>(player).is_some());
    }

    #[test]
    fn rent_with_a_trailing_space_in_an_inn_lists_tiers() {
        let (mut world, player, mut rx) = world_with_clerk(true);
        world.insert_resource(mud_world::SocialRegistry::default());
        world.entity_mut(player).insert(Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role: UserRole::Player,
            account_role: UserRole::Player,
            perms: vec![],
        });
        crate::commands::dispatch(&mut world, player, "rent ");
        let out = drain(&mut rx);
        assert!(
            out.contains("Available rooms here") && out.contains("offer"),
            "{out}"
        );
        assert!(world.get::<crate::commands::Quitting>(player).is_none());
    }

    #[test]
    fn rent_with_a_sleeping_receptionist_refuses() {
        let (mut world, player, mut rx) = world_with_clerk(false);
        let clerk = clerk_of(&mut world, player);
        world
            .entity_mut(clerk)
            .insert(Posture(PostureKind::Sleeping));
        cmd_rent(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("is unable to talk to you"), "{out}");
        assert!(world.get::<crate::commands::Quitting>(player).is_none());
        // Resting is awake enough.
        world
            .entity_mut(clerk)
            .insert(Posture(PostureKind::Resting));
        cmd_rent(&mut world, player, "");
        assert!(world.get::<crate::commands::Quitting>(player).is_some());
    }

    #[test]
    fn rent_with_a_busy_receptionist_refuses() {
        let (mut world, player, mut rx) = world_with_clerk(false);
        let clerk = clerk_of(&mut world, player);
        let room = world.get::<Located>(player).unwrap().0;
        let foe = world.spawn((Mob, Located(room))).id();
        world.entity_mut(clerk).insert(Fighting(foe));
        cmd_rent(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("too busy to help you"), "{out}");
        assert!(world.get::<crate::commands::Quitting>(player).is_none());
    }

    #[test]
    fn rent_with_an_invisible_player_is_refused_unless_staff() {
        let (mut world, player, mut rx) = world_with_clerk(false);
        world.entity_mut(player).insert(mud_world::Invisible);
        cmd_rent(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(
            out.contains("I don't deal with people I can't see"),
            "{out}"
        );
        assert!(world.get::<crate::commands::Quitting>(player).is_none());
        world.entity_mut(player).insert(Account {
            user_id: "u".into(),
            character_id: "c".into(),
            role: UserRole::Immortal,
            account_role: UserRole::Immortal,
            perms: vec![],
        });
        cmd_rent(&mut world, player, "");
        assert!(world.get::<crate::commands::Quitting>(player).is_some());
    }

    #[test]
    fn rent_away_from_a_receptionist_refuses_and_does_not_quit() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (player, mut rx) = test_support::player_in(&mut world, room);
        cmd_rent(&mut world, player, "");
        assert!(drain(&mut rx).contains("nothing to rent"));
        assert!(world.get::<crate::commands::Quitting>(player).is_none());
    }

    #[test]
    fn offer_reports_free_storage_and_lists_inn_tiers() {
        let (mut world, player, mut rx) = world_with_clerk(true);
        cmd_offer(&mut world, player, "");
        let out = drain(&mut rx);
        assert!(out.contains("No rent is due"), "{out}");
        assert!(out.contains("basic") && out.contains("5 gp"), "{out}");
    }

    #[test]
    fn offer_with_nobody_to_ask_refuses() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (player, mut rx) = test_support::player_in(&mut world, room);
        cmd_offer(&mut world, player, "");
        assert!(drain(&mut rx).contains("no one here"));
    }
}
