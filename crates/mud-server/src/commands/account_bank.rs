//! Account-shared bank commands. Distinct from the per-character
//! `balance` / `deposit` / `withdraw` family because the coin lives on
//! `Users.account_wealth` rather than `Characters.bank_wealth` — every
//! character on the same account sees the same pool, and a transfer by
//! one character is immediately visible to every other online sibling.
//!
//! Internal pattern:
//! * `cmd_account_balance` is a pure read off the `AccountWealth`
//!   component. No mutation; safe everywhere.
//! * `cmd_account_deposit` moves coin from the per-character bank into
//!   the shared pool. The opposite direction is `cmd_account_withdraw`.
//!   Neither touches on-hand wealth — players use the existing
//!   `deposit` / `withdraw` for that, then `adeposit` / `awithdraw` to
//!   shuttle into the shared pool.
//! * Cross-character sync: after the mutation, `fanout_account_wealth`
//!   walks every online character whose `Account.user_id` matches and
//!   updates their `AccountWealth` component. The DB write is
//!   fire-and-forget on tokio (same shape as the feedback command).

use bevy_ecs::prelude::*;
use mud_db::enums::UserRole;
use mud_world::{Account, AccountWealth, BankWealth, Online, Player};

use crate::commands::{Category, Command, DbPool, Help, format_wealth, send_to};

inventory::submit! {
    Command {
        names: &["account_balance", "abal", "accountbal", "accountbalance"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "account_balance",
            summary: "Show the account-shared bank balance.",
            long: "Read-only display of `Users.account_wealth`. \
                   Every character on this account sees the same \
                   pool; pair with `account_deposit` / \
                   `account_withdraw` to transfer between the \
                   per-character bank (`balance`) and the shared pool.",
        },
        run: cmd_account_balance,
    }
}

inventory::submit! {
    Command {
        names: &["account_deposit", "adeposit"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "account_deposit <amount>",
            summary: "Move copper from per-character bank to the shared pool.",
            long: "Source is your `bank` balance, not on-hand. \
                   Refuses if your per-character bank can't cover \
                   the amount. After success every online character \
                   on this account sees the new shared pool size.",
        },
        run: cmd_account_deposit,
    }
}

inventory::submit! {
    Command {
        names: &["account_withdraw", "awithdraw"],
        min_role: UserRole::Player,
        required_perm: None,
        category: Category::Banking,
        help: Help {
            usage: "account_withdraw <amount>",
            summary: "Move copper from the shared pool to your per-character bank.",
            long: "Inverse of `account_deposit`. Refuses if the \
                   shared pool can't cover the amount. Cross-character \
                   sync runs after the transfer — all sibling \
                   characters online refresh their view of the pool.",
        },
        run: cmd_account_withdraw,
    }
}

fn cmd_account_balance(world: &mut World, player: Entity, _args: &str) {
    let pool = world
        .get::<AccountWealth>(player)
        .map_or(0, |a| a.0);
    let line = format_wealth(pool).map_or_else(
        || "Your account chest is empty.".to_string(),
        |parts| format!("Your account shares hold {parts}."),
    );
    send_to(world, player, format!("\r\n{line}\r\n"));
}

fn cmd_account_deposit(world: &mut World, player: Entity, args: &str) {
    account_transfer(world, player, args, AccountDir::Deposit);
}

fn cmd_account_withdraw(world: &mut World, player: Entity, args: &str) {
    account_transfer(world, player, args, AccountDir::Withdraw);
}

#[derive(Clone, Copy)]
enum AccountDir {
    /// Per-character bank → shared pool.
    Deposit,
    /// Shared pool → per-character bank.
    Withdraw,
}

/// Shared body for `account_deposit` / `account_withdraw`. Pulls the
/// amount, validates the source can cover it, mutates both components
/// on the calling character, fans the new pool size out to every
/// other online sibling, and queues a fire-and-forget DB write.
///
/// The on-hand `Wealth` component is intentionally untouched —
/// players already have `deposit` / `withdraw` for the on-hand
/// ↔ per-character-bank step. Splitting the two transfers keeps
/// each command predictable about which side it touches.
fn account_transfer(world: &mut World, player: Entity, args: &str, direction: AccountDir) {
    let label = match direction {
        AccountDir::Deposit => "account_deposit",
        AccountDir::Withdraw => "account_withdraw",
    };
    let amount = match args.trim().parse::<i64>() {
        Ok(n) if n > 0 => n,
        _ => {
            send_to(
                world,
                player,
                format!("Usage: {label} <amount of copper>\r\n"),
            );
            return;
        }
    };
    let bank = world.get::<BankWealth>(player).map_or(0, |b| b.0);
    let account_pool = world
        .get::<AccountWealth>(player)
        .map_or(0, |a| a.0);
    match direction {
        AccountDir::Deposit if bank < amount => {
            send_to(
                world,
                player,
                "Your per-character bank doesn't have that much.\r\n",
            );
            return;
        }
        AccountDir::Withdraw if account_pool < amount => {
            send_to(
                world,
                player,
                "The account shares don't have that much.\r\n",
            );
            return;
        }
        _ => {}
    }
    // Compute the new pool *before* applying so we know what to fan
    // out to siblings. Both branches keep the per-character bank
    // and the shared pool in sync on the calling character first;
    // the fanout below patches the AccountWealth on every sibling.
    let (new_bank, new_account_pool) = match direction {
        AccountDir::Deposit => (bank - amount, account_pool + amount),
        AccountDir::Withdraw => (bank + amount, account_pool - amount),
    };
    if let Some(mut b) = world.get_mut::<BankWealth>(player) {
        b.0 = new_bank;
    }
    if let Some(mut a) = world.get_mut::<AccountWealth>(player) {
        a.0 = new_account_pool;
    }
    // user_id stamped on the calling character — every sibling
    // online shares the same id and needs the AccountWealth
    // component refreshed. Skip fanout when the player has no
    // Account component (shouldn't happen for a real player).
    let user_id = world
        .get::<Account>(player)
        .map(|a| a.user_id.clone());
    if let Some(uid) = user_id.clone() {
        fanout_account_wealth(world, &uid, new_account_pool, Some(player));
    }
    // Fire-and-forget DB write — the in-memory state already
    // reflects the transfer, so the player sees an immediate
    // response. If the DB write fails the next save tick covers it
    // because save_player also persists account_wealth.
    if let (Some(uid), Some(pool)) = (
        user_id,
        world.get_resource::<DbPool>().map(|p| p.0.clone()),
    ) {
        let new_pool = new_account_pool;
        tokio::spawn(async move {
            if let Err(e) =
                mud_db::users::save_account_wealth(&pool, &uid, new_pool).await
            {
                tracing::warn!(error = %e, user_id = %uid, "account_wealth save failed");
            }
        });
    }
    let verb = match direction {
        AccountDir::Deposit => "Moved",
        AccountDir::Withdraw => "Withdrew",
    };
    let suffix = match direction {
        AccountDir::Deposit => "to the account shares",
        AccountDir::Withdraw => "from the account shares",
    };
    send_to(
        world,
        player,
        format!("{verb} {amount} copper {suffix}.\r\n"),
    );
}

/// Refresh `AccountWealth` on every online character matching
/// `user_id`. Called after a per-character transfer so siblings on
/// the same account see the new shared pool size on their next
/// `account_balance` read — without a DB roundtrip.
///
/// `except` skips one entity (typically the caller) when the caller
/// already mutated its own component before computing the new value.
/// Public to the crate so tests can drive it directly.
pub(crate) fn fanout_account_wealth(
    world: &mut World,
    user_id: &str,
    new_pool: i64,
    except: Option<Entity>,
) {
    // Two-step: collect the entities under a borrow, then mutate.
    // `query_filtered` with `&Account` makes the user_id read cheap;
    // the With<Online> filter keeps disconnected entities out of
    // the fanout (their save path picks up the new value on next
    // login, since AccountWealth is loaded from Users.account_wealth).
    let targets: Vec<Entity> = {
        let mut q = world.query_filtered::<(Entity, &Account), (With<Player>, With<Online>)>();
        q.iter(world)
            .filter(|(e, acc)| {
                acc.user_id == user_id && Some(*e) != except
            })
            .map(|(e, _)| e)
            .collect()
    };
    for e in targets {
        if let Some(mut a) = world.get_mut::<AccountWealth>(e) {
            a.0 = new_pool;
        } else if let Ok(mut em) = world.get_entity_mut(e) {
            em.insert(AccountWealth(new_pool));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mud_db::enums::Permission;
    use mud_world::Named;

    /// Helper: spawn two characters on the same account, both online,
    /// each with their own BankWealth and AccountWealth. Returns
    /// (world, char_a, char_b, user_id).
    fn make_two_char_world() -> (World, Entity, Entity, String) {
        let mut world = World::new();
        let user_id = "user-123".to_string();
        let a = world
            .spawn((
                Player,
                Online,
                Named { name: "Alpha".to_string() },
                Account {
                    user_id: user_id.clone(),
                    character_id: "char-a".to_string(),
                    role: UserRole::Player,
                    perms: Vec::<Permission>::new(),
                },
                BankWealth(1000),
                AccountWealth(0),
            ))
            .id();
        let b = world
            .spawn((
                Player,
                Online,
                Named { name: "Beta".to_string() },
                Account {
                    user_id: user_id.clone(),
                    character_id: "char-b".to_string(),
                    role: UserRole::Player,
                    perms: Vec::<Permission>::new(),
                },
                BankWealth(500),
                AccountWealth(0),
            ))
            .id();
        (world, a, b, user_id)
    }

    #[test]
    fn deposit_moves_bank_to_account_pool() {
        // Per-character bank shrinks, shared pool grows. On-hand
        // Wealth (not modeled here) is untouched.
        let (mut world, a, _b, _uid) = make_two_char_world();
        account_transfer(&mut world, a, "300", AccountDir::Deposit);
        assert_eq!(world.get::<BankWealth>(a).unwrap().0, 700);
        assert_eq!(world.get::<AccountWealth>(a).unwrap().0, 300);
    }

    #[test]
    fn deposit_refuses_when_bank_short() {
        // Source side validation. State must not mutate on refusal.
        let (mut world, a, _b, _uid) = make_two_char_world();
        account_transfer(&mut world, a, "5000", AccountDir::Deposit);
        assert_eq!(world.get::<BankWealth>(a).unwrap().0, 1000);
        assert_eq!(world.get::<AccountWealth>(a).unwrap().0, 0);
    }

    #[test]
    fn withdraw_moves_account_pool_to_bank() {
        // Seed the pool then pull from it.
        let (mut world, a, _b, _uid) = make_two_char_world();
        if let Some(mut p) = world.get_mut::<AccountWealth>(a) {
            p.0 = 700;
        }
        account_transfer(&mut world, a, "200", AccountDir::Withdraw);
        assert_eq!(world.get::<BankWealth>(a).unwrap().0, 1200);
        assert_eq!(world.get::<AccountWealth>(a).unwrap().0, 500);
    }

    #[test]
    fn fanout_updates_sibling_account_pool() {
        // The core cross-character sync claim: a transfer on Alpha
        // updates Beta's AccountWealth component immediately, no DB
        // roundtrip required.
        let (mut world, a, b, _uid) = make_two_char_world();
        account_transfer(&mut world, a, "300", AccountDir::Deposit);
        assert_eq!(world.get::<AccountWealth>(a).unwrap().0, 300);
        assert_eq!(
            world.get::<AccountWealth>(b).unwrap().0,
            300,
            "sibling on same account should reflect new pool size"
        );
    }

    #[test]
    fn fanout_skips_other_users() {
        // A character on a different account should NOT be touched
        // by Alpha's transfer.
        let (mut world, a, _b, _uid) = make_two_char_world();
        let other = world
            .spawn((
                Player,
                Online,
                Named { name: "Other".to_string() },
                Account {
                    user_id: "different-user".to_string(),
                    character_id: "char-c".to_string(),
                    role: UserRole::Player,
                    perms: Vec::<Permission>::new(),
                },
                BankWealth(0),
                AccountWealth(0),
            ))
            .id();
        account_transfer(&mut world, a, "300", AccountDir::Deposit);
        assert_eq!(
            world.get::<AccountWealth>(other).unwrap().0,
            0,
            "different-account character should not see Alpha's deposit"
        );
    }
}
