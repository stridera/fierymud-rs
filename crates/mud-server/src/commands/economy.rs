//! Banker / shopkeeper service commands ported from the C++
//! `economy_commands.cpp`: `exchange` (money changer) and `repair`
//! (fuel refill; there is no durability system, matching C++).

use bevy_ecs::prelude::*;
use mud_db::enums::{MobProfession, UserRole};
use mud_world::{Item, LightFuel, Located, Mob, Shopkeeper, Wealth};

use crate::commands::{
    Category, Command, EquipFilter, Help, find_carried_by, format_wealth, is_staff, name_of,
    require_profession_in_room, send_to,
};

/// Light-source fuel cap (game hours) used when the item doesn't
/// carry its own `capacity`. Mirrors the C++ `MAX_LIGHT_DURATION`.
const DEFAULT_LIGHT_CAPACITY: i32 = 24;
/// Refill price: copper per game-hour of fuel.
const REFUEL_COPPER_PER_HOUR: i64 = 10;

inventory::submit! {
    Command {
        names: &["exchange"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "exchange <amount> <from-type> <to-type>",
            summary: "Exchange coins at a money changer (banker).",
            long: "Converts coins between denominations at a banker. \
                   Rates: 10 copper = 1 silver, 10 silver = 1 gold, \
                   10 gold = 1 platinum. Wealth is stored as a single \
                   copper total, so the exchange re-expresses your \
                   coins rather than changing your net worth; any \
                   remainder that doesn't fill a whole target coin is \
                   returned as change.",
        },
        run: cmd_exchange,
    }
}

inventory::submit! {
    Command {
        names: &["repair"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "repair <item>",
            summary: "Have a shopkeeper repair or refuel an item.",
            long: "Refills a lantern or torch for 10 copper per missing \
                   game-hour of fuel. Other items have no durability \
                   and are reported as already in perfect condition.",
        },
        run: cmd_repair,
    }
}

/// Coin denominations in copper. Ordered platinum..copper like the
/// C++ `COIN_DEFS` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Coin {
    Platinum,
    Gold,
    Silver,
    Copper,
}

impl Coin {
    /// Accepts the full name, the three-letter short name, or the
    /// single-letter initial (C++ `parse_coin_type`).
    fn parse(word: &str) -> Option<Self> {
        match word.to_ascii_lowercase().as_str() {
            "platinum" | "plat" | "p" => Some(Self::Platinum),
            "gold" | "gol" | "g" => Some(Self::Gold),
            "silver" | "sil" | "s" => Some(Self::Silver),
            "copper" | "cop" | "c" => Some(Self::Copper),
            _ => None,
        }
    }

    fn value(self) -> i64 {
        match self {
            Self::Platinum => 1000,
            Self::Gold => 100,
            Self::Silver => 10,
            Self::Copper => 1,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Platinum => "platinum",
            Self::Gold => "gold",
            Self::Silver => "silver",
            Self::Copper => "copper",
        }
    }
}

/// Result of pricing an exchange: total copper spent, copper received
/// as whole target coins, and the leftover returned as change.
#[derive(Debug, PartialEq, Eq)]
struct ExchangeQuote {
    spent: i64,
    /// Whole target coins handed over.
    coins: i64,
    received: i64,
    change: i64,
}

fn quote_exchange(amount: i64, from: Coin, to: Coin) -> Option<ExchangeQuote> {
    let spent = amount.checked_mul(from.value())?;
    let coins = spent / to.value();
    if coins == 0 {
        return None;
    }
    Some(ExchangeQuote {
        spent,
        coins,
        received: coins * to.value(),
        change: spent % to.value(),
    })
}

fn cmd_exchange(world: &mut World, player: Entity, args: &str) {
    if !require_profession_in_room(world, player, MobProfession::Banker, "banker") {
        return;
    }
    let tokens: Vec<&str> = args.split_whitespace().collect();
    if tokens.len() < 3 {
        send_to(
            world,
            player,
            "Usage: exchange <amount> <from-type> <to-type>\r\n\
             Example: exchange 10 gold silver\r\n\
             Available types: copper, silver, gold, platinum\r\n\
             Rates: 10 copper = 1 silver, 10 silver = 1 gold, 10 gold = 1 platinum\r\n",
        );
        return;
    }
    let Ok(amount) = tokens[0].parse::<i64>() else {
        send_to(world, player, "Invalid amount specified.\r\n");
        return;
    };
    if amount <= 0 {
        send_to(world, player, "You must exchange at least 1 coin.\r\n");
        return;
    }
    let (Some(from), Some(to)) = (Coin::parse(tokens[1]), Coin::parse(tokens[2])) else {
        send_to(
            world,
            player,
            "Invalid coin type. Use: copper, silver, gold, or platinum.\r\n",
        );
        return;
    };
    if from == to {
        send_to(
            world,
            player,
            "You can't exchange a coin type for itself.\r\n",
        );
        return;
    }
    let Some(quote) = quote_exchange(amount, from, to) else {
        // Either overflow or not enough to fill one whole target coin.
        send_to(
            world,
            player,
            format!("That's not enough to exchange into {}.\r\n", to.name()),
        );
        return;
    };
    let on_hand = world.get::<Wealth>(player).map_or(0, |w| w.0);
    if on_hand < quote.spent {
        send_to(
            world,
            player,
            format!("You don't have {amount} {}.\r\n", from.name()),
        );
        return;
    }
    // Wealth is a single copper total: spending `spent` and receiving
    // `received + change` nets to zero, so there is nothing to write
    // back. Only the denominations shown to the player change.
    let mut out = format!(
        "The banker exchanges {amount} {} for {} {}.\r\n",
        from.name(),
        quote.coins,
        to.name()
    );
    if quote.change > 0 {
        let change_txt = format_wealth(quote.change).unwrap_or_default();
        out.push_str(&format!("You receive {change_txt} in change.\r\n"));
    }
    send_to(world, player, out);
}

/// First shopkeeper mob sharing the player's room.
fn shopkeeper_here(world: &mut World, player: Entity) -> Option<Entity> {
    let room = world.get::<Located>(player)?.0;
    let mut q = world.query_filtered::<(Entity, &Located), (With<Mob>, With<Shopkeeper>)>();
    q.iter(world).find(|(_, l)| l.0 == room).map(|(e, _)| e)
}

fn cmd_repair(world: &mut World, player: Entity, args: &str) {
    let needle = args.trim();
    if needle.is_empty() {
        send_to(
            world,
            player,
            "Repair what item?\r\nUsage: repair <item>\r\n",
        );
        return;
    }
    let Some(keeper) = shopkeeper_here(world, player) else {
        send_to(
            world,
            player,
            "You need to find a shopkeeper to repair items.\r\n",
        );
        return;
    };
    let Some(item) = find_carried_by(world, needle, player, EquipFilter::Anywhere) else {
        send_to(world, player, format!("You don't have '{needle}'.\r\n"));
        return;
    };
    let keeper_name = name_of(world, keeper);
    let item_name = name_of(world, item);
    let mut out = format!("{keeper_name} examines {item_name}...\r\n");

    let fuel = world
        .get::<LightFuel>(item)
        .copied()
        .filter(|_| world.get::<Item>(item).is_some());
    let Some(fuel) = fuel else {
        // No durability model — same verdict as the C++ server.
        out.push_str(&format!(
            "{keeper_name} tells you, 'This item is in perfect condition. No repairs needed.'\r\n"
        ));
        send_to(world, player, out);
        return;
    };
    if fuel.remaining < 0 || fuel.capacity < 0 {
        out.push_str(&format!(
            "{keeper_name} tells you, 'This {item_name} never runs out of fuel.'\r\n"
        ));
        send_to(world, player, out);
        return;
    }
    let max = if fuel.capacity > 0 {
        fuel.capacity
    } else {
        DEFAULT_LIGHT_CAPACITY
    };
    if fuel.remaining >= max {
        out.push_str(&format!(
            "{keeper_name} tells you, 'This {item_name} is already fully fueled.'\r\n"
        ));
        send_to(world, player, out);
        return;
    }
    let hours = max - fuel.remaining;
    let cost = i64::from(hours) * REFUEL_COPPER_PER_HOUR;
    let cost_txt = format_wealth(cost).unwrap_or_default();
    if !is_staff(world, player) {
        let on_hand = world.get::<Wealth>(player).map_or(0, |w| w.0);
        if on_hand < cost {
            out.push_str(&format!(
                "{keeper_name} tells you, 'That will cost {cost_txt} to refill. You don't have enough.'\r\n"
            ));
            send_to(world, player, out);
            return;
        }
        if let Some(mut w) = world.get_mut::<Wealth>(player) {
            w.0 -= cost;
        }
        out.push_str(&format!("{keeper_name} takes {cost_txt} for the fuel.\r\n"));
    }
    if let Some(mut f) = world.get_mut::<LightFuel>(item) {
        f.remaining = max;
    }
    out.push_str(&format!(
        "{keeper_name} refills {item_name} with fresh oil.\r\n\
         Your {item_name} now has {max} hours of fuel.\r\n"
    ));
    send_to(world, player, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{Rx, drain, mob_proto, player_in};
    use mud_world::{Keywords, MobPrototypes, Named, WorldKey};

    /// Room with a banker; the player holds `wealth` copper.
    fn banker_world(wealth: i64) -> (World, Entity, Rx) {
        let mut world = World::new();
        let mut protos = MobPrototypes::default();
        protos
            .by_key
            .insert((1, 1), mob_proto(1, 1, MobProfession::Banker));
        world.insert_resource(protos);
        let room = world.spawn_empty().id();
        world.spawn((
            Mob,
            Named {
                name: "a thin banker".to_string(),
            },
            Located(room),
            WorldKey { zone: 1, id: 1 },
        ));
        let (player, rx) = player_in(&mut world, room);
        world.entity_mut(player).insert(Wealth(wealth));
        (world, player, rx)
    }

    /// Room with a shopkeeper mob; returns the (room, player, rx).
    fn shop_world(wealth: i64) -> (World, Entity, Rx) {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        world.spawn((
            Mob,
            Named {
                name: "Old Tom".to_string(),
            },
            Located(room),
            Shopkeeper {
                shop_zone_id: 1,
                shop_id: 1,
            },
        ));
        let (player, rx) = player_in(&mut world, room);
        world.entity_mut(player).insert(Wealth(wealth));
        (world, player, rx)
    }

    #[test]
    fn quote_converts_and_returns_change() {
        // 15 silver -> gold: 150 copper = 1 gold + 50 copper change.
        let q = quote_exchange(15, Coin::Silver, Coin::Gold).unwrap();
        assert_eq!(
            q,
            ExchangeQuote {
                spent: 150,
                coins: 1,
                received: 100,
                change: 50
            }
        );
        // Too little to fill one platinum.
        assert!(quote_exchange(5, Coin::Gold, Coin::Platinum).is_none());
        // Going down never leaves change.
        let q = quote_exchange(2, Coin::Gold, Coin::Copper).unwrap();
        assert_eq!((q.received, q.change), (200, 0));
    }

    #[test]
    fn exchange_requires_banker() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (player, mut rx) = player_in(&mut world, room);
        world.entity_mut(player).insert(Wealth(500));
        cmd_exchange(&mut world, player, "10 gold silver");
        assert!(drain(&mut rx).contains("You need a banker here"));
        assert_eq!(world.get::<Wealth>(player).unwrap().0, 500);
    }

    #[test]
    fn exchange_reports_conversion_and_keeps_net_worth() {
        let (mut world, player, mut rx) = banker_world(1000);
        cmd_exchange(&mut world, player, "10 silver gold");
        let out = drain(&mut rx);
        assert!(
            out.contains("The banker exchanges 10 silver for 1 gold."),
            "{out}"
        );
        assert_eq!(world.get::<Wealth>(player).unwrap().0, 1000);
    }

    #[test]
    fn exchange_refuses_when_broke() {
        let (mut world, player, mut rx) = banker_world(5);
        cmd_exchange(&mut world, player, "1 gold silver");
        assert!(drain(&mut rx).contains("You don't have 1 gold."));
    }

    #[test]
    fn repair_needs_shopkeeper() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let (player, mut rx) = player_in(&mut world, room);
        cmd_repair(&mut world, player, "lantern");
        assert!(drain(&mut rx).contains("find a shopkeeper"));
    }

    #[test]
    fn repair_refuels_lantern_and_charges_copper() {
        let (mut world, player, mut rx) = shop_world(1000);
        let lantern = world
            .spawn((
                Item,
                Named {
                    name: "a lantern".to_string(),
                },
                Keywords(vec!["lantern".to_string()]),
                Located(player),
                LightFuel {
                    capacity: 24,
                    remaining: 20,
                },
            ))
            .id();
        cmd_repair(&mut world, player, "lantern");
        let out = drain(&mut rx);
        assert!(out.contains("refills a lantern"), "{out}");
        assert_eq!(world.get::<LightFuel>(lantern).unwrap().remaining, 24);
        // 4 missing hours * 10 copper.
        assert_eq!(world.get::<Wealth>(player).unwrap().0, 960);

        // Now full: no further charge.
        cmd_repair(&mut world, player, "lantern");
        assert!(drain(&mut rx).contains("already fully fueled"));
        assert_eq!(world.get::<Wealth>(player).unwrap().0, 960);
    }

    #[test]
    fn repair_plain_item_is_perfect() {
        let (mut world, player, mut rx) = shop_world(0);
        world.spawn((
            Item,
            Named {
                name: "a sword".to_string(),
            },
            Keywords(vec!["sword".to_string()]),
            Located(player),
        ));
        cmd_repair(&mut world, player, "sword");
        assert!(drain(&mut rx).contains("perfect condition"));
    }
}
