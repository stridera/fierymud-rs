//! Rest / repose: `rent` command.
//!
//! Inn rental flow (R2):
//! - `rent` with no arg in an `is_inn` room: print the available tier
//!   menu pulled from the room's `InnRoom` component.
//! - `rent <tier-name>`: validate the name, check gold, and either
//!   set the RestSource to `Inn` immediately (tier 1) or stash a
//!   `PendingRentConfirm` component and prompt the player to confirm
//!   (tier > 1).
//! - Outside an `is_inn` room: "There's nothing to rent here."
//!
//! Per ADR 0001 §3 the fee is **flat per tier**, NOT per-night. The
//! player who returns in a year pays the same as the player who
//! returns in an hour. Per the design doc's edge-case table,
//! renting downgrade after an existing higher-tier rent overwrites
//! the source with no refund — caveat emptor.

use bevy_ecs::prelude::*;
use mud_db::enums::{RestSource, UserRole};
use mud_world::{InnRoom, Located, RestState, Wealth};

use crate::commands::{Category, Command, Help, send_rendered, send_to};

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
            summary: "Rent a room at an inn for a rest / repose bonus.",
            long: "In an inn room (one flagged `is_inn` by builders), \
                   `rent` with no argument lists the available tiers \
                   and their fees. `rent <name>` charges the fee in \
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
    let inn = world.get::<InnRoom>(room).cloned();
    let Some(inn) = inn else {
        send_to(world, player, "There's nothing to rent here.\r\n");
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
    out.push_str("Use `rent <name>` to book one. Tiers 2 and 3 ask to confirm.\r\n");
    send_to(world, player, out);
}
